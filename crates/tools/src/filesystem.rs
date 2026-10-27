use crate::{Result, ToolError, WorkspaceTools, WriteInput, WriteMode};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
#[cfg(unix)]
use cap_std::fs::{MetadataExt, OpenOptionsExt};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::{Component, Path},
};

impl WorkspaceTools {
    fn parent(&self, path: &str) -> Result<(Dir, String)> {
        let mut parts = components(path)?;
        let leaf = parts
            .pop()
            .ok_or(ToolError::Invalid("file path required"))?;
        let mut directory = self.root.try_clone()?;
        for component in parts {
            directory = directory.open_dir_nofollow(component)?;
        }
        Ok((directory, leaf))
    }

    pub(crate) fn read(&self, path: &str) -> Result<Value> {
        let (directory, leaf) = self.parent(path)?;
        let mut options = regular_options();
        options.read(true);
        let file = directory.open_with(leaf, &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(ToolError::Invalid("only regular files can be read"));
        }
        if metadata.len() > self.limits.read_bytes as u64 {
            return Err(ToolError::Limit("read bytes"));
        }
        let mut bytes = Vec::new();
        file.take(self.limits.read_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > self.limits.read_bytes {
            return Err(ToolError::Limit("read bytes"));
        }
        let content =
            String::from_utf8(bytes).map_err(|_| ToolError::Invalid("file is not UTF-8"))?;
        Ok(json!({"content":content}))
    }

    pub(crate) fn write(&self, args: WriteInput) -> Result<Value> {
        let granted = match args.mode {
            WriteMode::Create => self.grants.create,
            WriteMode::Overwrite => self.grants.overwrite,
        };
        if !granted {
            return Err(ToolError::Denied);
        }
        if args.content.len() > self.limits.write_bytes {
            return Err(ToolError::Limit("write bytes"));
        }
        let (directory, leaf) = self.parent(&args.path)?;
        let mut options = regular_options();
        options.write(true);
        if matches!(args.mode, WriteMode::Create) {
            options.create_new(true);
        }
        let mut file = directory.open_with(leaf, &options)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(ToolError::Invalid("only regular files can be written"));
        }
        #[cfg(unix)]
        if metadata.nlink() != 1 {
            return Err(ToolError::Invalid(
                "multiply linked files cannot be written",
            ));
        }
        // Validate the opened object before truncating. Overwrite never creates a missing file.
        if matches!(args.mode, WriteMode::Overwrite) {
            file.set_len(0)?;
        }
        file.write_all(args.content.as_bytes())?;
        file.sync_all()?;
        Ok(json!({"bytes_written":args.content.len()}))
    }

    pub(crate) fn list(&self, path: &str) -> Result<Value> {
        let mut directory = self.root.try_clone()?;
        if path != "." {
            for component in components(path)? {
                directory = directory.open_dir_nofollow(component)?;
            }
        }
        let mut entries = Vec::new();
        let mut output_bytes = br#"{"entries":[]}"#.len();
        for entry in directory.entries()? {
            let entry = entry?;
            if entries.len() == self.limits.entries {
                return Err(ToolError::Limit("directory entries"));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ToolError::Invalid("entry name is not UTF-8"))?;
            let kind = entry.file_type()?;
            let kind = if kind.is_symlink() {
                "symlink"
            } else if kind.is_file() {
                "file"
            } else if kind.is_dir() {
                "directory"
            } else {
                "other"
            };
            let value = json!({"name":name,"kind":kind});
            let encoded = serde_json::to_vec(&value)
                .map_err(|_| ToolError::Invalid("cannot encode entry"))?;
            output_bytes += encoded.len() + usize::from(!entries.is_empty());
            if output_bytes > self.limits.output_bytes {
                return Err(ToolError::Limit("output bytes"));
            }
            entries.push(value);
        }
        entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({"entries":entries}))
    }
}

fn components(path: &str) -> Result<Vec<String>> {
    if path.is_empty()
        || path.len() > 4096
        || path.contains(['\0', '\\', ':'])
        || path.split('/').any(|part| part == "." || part == "..")
    {
        return Err(ToolError::Invalid(
            "path must be a bounded relative workspace path",
        ));
    }
    let mut parts = Vec::new();
    for component in Path::new(path).components() {
        match component {
            Component::Normal(name) => parts.push(
                name.to_str()
                    .ok_or(ToolError::Invalid("path must be UTF-8"))?
                    .into(),
            ),
            _ => {
                return Err(ToolError::Invalid(
                    "absolute paths, dot and parent traversal are denied",
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(ToolError::Invalid("path must name a workspace entry"));
    }
    Ok(parts)
}
fn regular_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NONBLOCK);
        options.mode(0o600);
    }
    options
}
