use super::*;
pub(super) fn text(text: &str) -> Result<()> {
    if text
        .chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\r' | '\n'))
    {
        return Err(AttachmentError::InvalidText);
    }
    Ok(())
}
pub(super) fn batch(files: &[TextAttachment]) -> Result<usize> {
    if files.len() > MAX_FILES {
        return Err(AttachmentError::Limit);
    }
    let mut total = 0;
    for file in files {
        if file.content.len() > MAX_FILE_BYTES {
            return Err(AttachmentError::Limit);
        }
        text(&file.content)?;
        total += file.content.len();
        if total > MAX_TOTAL_BYTES {
            return Err(AttachmentError::Limit);
        }
    }
    Ok(total)
}
