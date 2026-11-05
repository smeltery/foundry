//! In-place text replacement under the read and overwrite grants. Resolution,
//! symlink refusal, link-count checks and byte limits are the same as `write`.
//! Like an overwrite, the rewrite is not transactional: an I/O failure after
//! truncation can leave the file empty or partial, so an error is never a
//! clean no-op and callers must reconcile before retrying.
use crate::{Result, ToolError, WorkspaceTools, filesystem::regular_options};
#[cfg(unix)]
use cap_std::fs::MetadataExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Seek, SeekFrom, Write};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EditInput {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

impl WorkspaceTools {
    pub(crate) fn edit(&self, args: EditInput) -> Result<Value> {
        // Edit rewrites an existing file and never creates one. Its match result
        // reveals file contents, so it also needs read.
        if !(self.grants.read && self.grants.overwrite) {
            return Err(ToolError::Denied);
        }
        if args.old_text.is_empty() {
            return Err(ToolError::Invalid("old_text must not be empty"));
        }
        let (directory, leaf) = self.parent(&args.path)?;
        let mut options = regular_options();
        options.read(true).write(true);
        let mut file = directory.open_with(leaf, &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(ToolError::Invalid("only regular files can be edited"));
        }
        #[cfg(unix)]
        if metadata.nlink() != 1 {
            return Err(ToolError::Invalid("multiply linked files cannot be edited"));
        }
        if metadata.len() > self.limits.read_bytes as u64 {
            return Err(ToolError::Limit("read bytes"));
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(self.limits.read_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > self.limits.read_bytes {
            return Err(ToolError::Limit("read bytes"));
        }
        let content =
            String::from_utf8(bytes).map_err(|_| ToolError::Invalid("file is not UTF-8"))?;
        let replacements = content.matches(&args.old_text).count();
        if replacements == 0 {
            return Err(ToolError::Invalid("old_text was not found"));
        }
        if replacements > 1 && !args.replace_all {
            return Err(ToolError::Invalid("old_text is not unique"));
        }
        // Bound the result before building it: many matches with a long
        // replacement could otherwise allocate far beyond the write limit.
        let projected = replacements
            .checked_mul(args.new_text.len())
            .and_then(|added| {
                (content.len() - replacements * args.old_text.len()).checked_add(added)
            });
        if projected.is_none_or(|bytes| bytes > self.limits.write_bytes) {
            return Err(ToolError::Limit("write bytes"));
        }
        let edited = content.replace(&args.old_text, &args.new_text);
        // Every check passed on the opened descriptor; only now rewrite that object.
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(edited.as_bytes())?;
        file.sync_all()?;
        Ok(json!({"replacements":replacements,"bytes_written":edited.len()}))
    }
}
