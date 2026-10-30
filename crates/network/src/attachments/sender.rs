use super::*;
use cap_fs_ext::DirExt;
use cap_std::{ambient_authority, fs::Dir};
use std::{io::Read, path::Component};

fn walk(root: &Dir, relative: &Path) -> Result<std::fs::File> {
    let mut directory = root.try_clone()?;
    let pieces: Vec<_> = relative
        .components()
        .map(|c| match c {
            Component::Normal(value) => value.to_str().ok_or(AttachmentError::UnsafePath),
            _ => Err(AttachmentError::UnsafePath),
        })
        .collect::<Result<_>>()?;
    let (last, parents) = pieces.split_last().ok_or(AttachmentError::UnsafePath)?;
    for name in parents {
        directory = directory
            .open_dir_nofollow(name)
            .map_err(|_| AttachmentError::UnsafePath)?;
    }
    filesystem::open_file(&directory, last, false)
}
fn prepare_one(policy: &SenderPolicy, given: &Path) -> Result<TextAttachment> {
    prepare_with(policy, given, |_| {})
}
fn prepare_with(
    policy: &SenderPolicy,
    given: &Path,
    before_open: impl FnOnce(&Path),
) -> Result<TextAttachment> {
    use std::os::unix::fs::MetadataExt;
    let selected = policy.workspace.join(given);
    let actual = selected
        .canonicalize()
        .map_err(|_| AttachmentError::UnsafePath)?;
    let enabled = policy
        .folders_on
        .iter()
        .filter(|p| actual.starts_with(p))
        .map(|p| p.components().count())
        .max()
        .unwrap_or(0);
    let disabled = policy
        .folders_off
        .iter()
        .filter(|p| actual.starts_with(p))
        .map(|p| p.components().count())
        .max()
        .unwrap_or(0);
    if disabled > 0 && disabled >= enabled {
        return Err(AttachmentError::UnsafePath);
    }
    let snapshot = std::fs::symlink_metadata(&actual)?;
    before_open(&actual);
    let mut file = if policy.mode == SenderMode::Agent {
        let relative = actual
            .strip_prefix(&policy.workspace)
            .map_err(|_| AttachmentError::UnsafePath)?;
        if !policy.allowed.iter().any(|p| actual.starts_with(p))
            || policy.denied.iter().any(|p| actual.starts_with(p))
            || relative
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            return Err(AttachmentError::UnsafePath);
        }
        let mut root = Dir::open_ambient_dir("/", ambient_authority())?;
        for component in policy
            .workspace
            .strip_prefix("/")
            .map_err(|_| AttachmentError::UnsafePath)?
            .components()
        {
            if let Component::Normal(part) = component {
                root = root
                    .open_dir_nofollow(part)
                    .map_err(|_| AttachmentError::UnsafePath)?;
            } else {
                return Err(AttachmentError::UnsafePath);
            }
        }
        walk(&root, relative)?
    } else {
        let root = Dir::open_ambient_dir("/", ambient_authority())?;
        walk(
            &root,
            actual
                .strip_prefix("/")
                .map_err(|_| AttachmentError::UnsafePath)?,
        )?
    };
    let held = file.metadata()?;
    // Sender authority is path-based; other names for this inode do not change it.
    if !held.is_file() || (held.dev(), held.ino()) != (snapshot.dev(), snapshot.ino()) {
        return Err(AttachmentError::UnsafePath);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(AttachmentError::Limit);
    }
    // A retargeted alias never changes the descriptor read; fail rather than return stale selection.
    if selected.canonicalize()? != actual {
        return Err(AttachmentError::UnsafePath);
    }
    let naming_path = if policy.mode == SenderMode::Agent {
        &actual
    } else {
        &selected
    };
    let name = naming_path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(AttachmentError::UnsafePath)?;
    Ok(TextAttachment {
        name: name.into(),
        content: String::from_utf8(bytes).map_err(|_| AttachmentError::InvalidText)?,
        path: None,
    })
}
pub(super) fn prepare(policy: &SenderPolicy, paths: &[PathBuf]) -> Result<Vec<TextAttachment>> {
    if paths.len() > MAX_FILES {
        return Err(AttachmentError::Limit);
    }
    let files = paths
        .iter()
        .map(|p| prepare_one(policy, p))
        .collect::<Result<Vec<_>>>()?;
    names::normalize_with(&files, &policy.frames)
}

#[cfg(test)]
#[path = "sender_tests.rs"]
mod tests;
