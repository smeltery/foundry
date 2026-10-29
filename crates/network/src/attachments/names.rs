use super::*;
use std::collections::BTreeSet;

fn omitted(c: char) -> bool {
    let n = c as u32;
    [
        0xad, 0x34f, 0x61c, 0x115f, 0x1160, 0x180e, 0x3164, 0xfeff, 0xffa0, 0x2028, 0x2029,
    ]
    .contains(&n)
        || [
            (0x200b, 0x200f),
            (0x202a, 0x202e),
            (0x2060, 0x206f),
            (0xfe00, 0xfe0f),
            (0xe0000, 0xe007f),
        ]
        .iter()
        .any(|(a, b)| (*a..=*b).contains(&n))
}
fn device(value: &str) -> bool {
    let head = value
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_uppercase();
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&head.as_str()) {
        return true;
    }
    ["COM", "LPT"].iter().any(|prefix| {
        head.strip_prefix(prefix).is_some_and(|tail| {
            let mut chars = tail.chars();
            chars
                .next()
                .is_some_and(|c| c.is_ascii_digit() || ['¹', '²', '³'].contains(&c))
                && chars.next().is_none()
        })
    })
}
pub(super) fn numbered(value: &str, number: Option<usize>) -> String {
    let extension = value
        .rfind('.')
        .filter(|i| *i > 0 && value[*i..].chars().count() <= 16)
        .unwrap_or(value.len());
    let (stem, tail) = value.split_at(extension);
    let suffix = number.map(|n| format!("-{n}")).unwrap_or_default();
    let byte_room = 200 - suffix.len() - tail.len();
    let char_room = 100 - suffix.chars().count() - tail.chars().count();
    let end = stem
        .char_indices()
        .enumerate()
        .take_while(|(count, (i, c))| *count < char_room && i + c.len_utf8() <= byte_room)
        .last()
        .map_or(0, |(_, (i, c))| i + c.len_utf8());
    let stem = if end < stem.len() {
        stem[..end].trim_end_matches(['.', ' '])
    } else {
        stem
    };
    format!("{stem}{suffix}{tail}")
}
/// Portable component normalization; text remains Unicode, never a peer path.
pub fn safe_name(value: &str) -> String {
    safe_with_frames(value, &[])
}
pub(super) fn safe_with_frames(value: &str, words: &[String]) -> String {
    let mut cooked = String::new();
    let mut dot = false;
    for c in value.chars().filter(|c| !omitted(*c)) {
        let c = if c.is_control() || "/\\:<>\"|?*".contains(c) {
            '_'
        } else {
            c
        };
        if c != '.' || !dot {
            cooked.push(c);
        }
        dot = c == '.';
    }
    let clean = cooked
        .trim_start_matches(|c: char| c == '.' || c == '-' || c.is_whitespace())
        .trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let mut result = numbered(clean, None);
    result = frame_names::defang(&result, words);
    if device(&result) {
        result = numbered(&format!("_{result}"), None);
    }
    if result.is_empty() {
        "file".into()
    } else {
        result
    }
}
pub(super) fn normalize(files: &[TextAttachment]) -> Result<Vec<TextAttachment>> {
    normalize_with(files, &[])
}
pub(super) fn normalize_with(
    files: &[TextAttachment],
    words: &[String],
) -> Result<Vec<TextAttachment>> {
    validation::batch(files)?;
    let mut used = BTreeSet::new();
    let mut output = Vec::with_capacity(files.len());
    for file in files {
        let base = safe_with_frames(&file.name, words);
        let mut selected = base.clone();
        let mut counter = 2;
        while !used.insert(selected.to_lowercase()) {
            selected = numbered(&base, Some(counter));
            counter += 1;
        }
        output.push(TextAttachment {
            name: selected,
            content: file.content.clone(),
            path: None,
        });
    }
    Ok(output)
}
pub(super) fn component(value: &str) -> bool {
    !value.contains(['/', '\\']) && value != "." && value != ".." && safe_name(value) == value
}
