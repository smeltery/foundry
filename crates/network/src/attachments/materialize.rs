use super::*;
use cap_std::fs::Dir;
use std::io::{Read, Write};

fn read(file: &mut std::fs::File) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(AttachmentError::Conflict);
    }
    Ok(bytes)
}
fn finish_part(dir: &Dir, part: &str, expected: &TextAttachment) -> Result<()> {
    let mut file = if filesystem::exists(dir, part)? {
        filesystem::open_file(dir, part, true)?
    } else {
        filesystem::create_file(dir, part)?
    };
    filesystem::private(&file)?;
    let bytes = read(&mut file)?;
    if !expected.content.as_bytes().starts_with(&bytes) {
        return Err(AttachmentError::Conflict);
    }
    // Resume only an exact prefix in our owned partial file, never truncate an occupant.
    file.write_all(&expected.content.as_bytes()[bytes.len()..])?;
    filesystem::mark_download(&file)?;
    file.sync_all()?;
    dir.hard_link(part, dir, &expected.name)?;
    dir.remove_file(part)?;
    Ok(())
}
fn verify_existing(dir: &Dir, part: &str, expected: &TextAttachment) -> Result<()> {
    let mut file = filesystem::open_file(dir, &expected.name, false)?;
    // A crash between hard-link publication and partial unlink leaves two links.
    if filesystem::exists(dir, part)? {
        use std::os::unix::fs::MetadataExt;
        let pending = filesystem::open_file(dir, part, false)?;
        let a = file.metadata()?;
        let b = pending.metadata()?;
        if (a.dev(), a.ino()) != (b.dev(), b.ino()) || a.nlink() != 2 {
            return Err(AttachmentError::Conflict);
        }
        if read(&mut file)? != expected.content.as_bytes() {
            return Err(AttachmentError::Conflict);
        }
        dir.remove_file(part)?;
        file = filesystem::open_file(dir, &expected.name, false)?;
    }
    filesystem::private(&file)?;
    if read(&mut file)? != expected.content.as_bytes() {
        return Err(AttachmentError::Conflict);
    }
    Ok(())
}
pub(super) fn resolve(
    dir: &Dir,
    files: &[TextAttachment],
    fixed: &[String],
) -> Result<Vec<String>> {
    if !fixed.is_empty() {
        return Ok(fixed.to_vec());
    }
    let mut selected = Vec::new();
    for file in files {
        let mut name = file.name.clone();
        for n in 2..=101 {
            let collision = if filesystem::exists(dir, &name)? {
                use cap_std::fs::MetadataExt;
                let candidate = dir.symlink_metadata(&name)?;
                selected.iter().any(|prior| {
                    dir.symlink_metadata(prior)
                        .is_ok_and(|m| (m.dev(), m.ino()) == (candidate.dev(), candidate.ino()))
                })
            } else {
                false
            };
            if !collision {
                break;
            }
            if n == 101 {
                return Err(AttachmentError::Conflict);
            }
            name = names::numbered(&file.name, Some(n));
        }
        selected.push(name);
    }
    Ok(selected)
}
pub(super) fn files(
    root: &Dir,
    root_path: &Path,
    key: &AttachmentKey,
    files: &[TextAttachment],
    fixed: &[String],
) -> Result<(Vec<TextAttachment>, Vec<String>)> {
    let id = key.directory()?;
    let dir = filesystem::directory(root, &id, true)?;
    let mut located = Vec::new();
    let mut selected = Vec::new();
    for (i, original) in files.iter().enumerate() {
        let actual = if fixed.is_empty() {
            let mut trial = files[..=i].to_vec();
            for (f, n) in trial.iter_mut().zip(&selected) {
                f.name.clone_from(n);
            }
            resolve(&dir, &trial, &[])?
                .pop()
                .ok_or(AttachmentError::Corrupt)?
        } else {
            fixed[i].clone()
        };
        let attachment = &TextAttachment {
            name: actual.clone(),
            content: original.content.clone(),
            path: None,
        };
        selected.push(actual);
        let part = format!(".part-{i}");
        if filesystem::exists(&dir, &attachment.name)? {
            verify_existing(&dir, &part, attachment)?;
        } else {
            finish_part(&dir, &part, attachment)?;
            verify_existing(&dir, &part, attachment)?;
        }
        located.push(TextAttachment {
            name: attachment.name.clone(),
            content: String::new(),
            path: Some(
                root_path
                    .join(&id)
                    .join(&attachment.name)
                    .to_str()
                    .ok_or(AttachmentError::UnsafePath)?
                    .into(),
            ),
        });
    }
    filesystem::sync(&dir)?;
    filesystem::sync(root)?;
    Ok((located, selected))
}
pub(super) fn delete(
    root: &Dir,
    key: &AttachmentKey,
    files: &[TextAttachment],
    actual: &[String],
) -> Result<()> {
    let id = key.directory()?;
    if !filesystem::exists(root, &id)? {
        return Ok(());
    }
    let dir = filesystem::directory(root, &id, false)?;
    let resolved = resolve(&dir, files, actual)?;
    let files: Vec<_> = files
        .iter()
        .zip(resolved)
        .map(|(f, name)| TextAttachment {
            name,
            content: f.content.clone(),
            path: None,
        })
        .collect();
    // Validate every remaining occupant before removing anything. Unknown entries stay.
    let mut expected = std::collections::BTreeSet::new();
    for (i, file) in files.iter().enumerate() {
        expected.insert(file.name.clone());
        expected.insert(format!(".part-{i}"));
    }
    for entry in dir.entries()? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| AttachmentError::UnsafePath)?;
        if !expected.contains(&name) {
            return Err(AttachmentError::Conflict);
        }
    }
    for (i, expected_file) in files.iter().enumerate() {
        let part = format!(".part-{i}");
        if filesystem::exists(&dir, &expected_file.name)? {
            verify_existing(&dir, &part, expected_file)?;
        } else if filesystem::exists(&dir, &part)? {
            let mut partial = filesystem::open_file(&dir, &part, false)?;
            filesystem::private(&partial)?;
            if !expected_file
                .content
                .as_bytes()
                .starts_with(&read(&mut partial)?)
            {
                return Err(AttachmentError::Conflict);
            }
        }
    }
    for name in expected {
        if filesystem::exists(&dir, &name)? {
            dir.remove_file(&name)?;
        }
    }
    filesystem::sync(&dir)?;
    root.remove_dir(&id)?;
    filesystem::sync(root)
}
