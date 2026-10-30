use super::*;
pub(super) fn validate(words: &[String]) -> Result<()> {
    if words.len() > 16
        || words
            .iter()
            .any(|w| w.is_empty() || w.len() > 32 || !w.bytes().all(|c| c.is_ascii_alphabetic()))
    {
        return Err(AttachmentError::Invalid);
    }
    Ok(())
}
fn spells(input: &[char], expected: &str) -> bool {
    let letters: Vec<_> = expected.to_ascii_lowercase().chars().collect();
    let mut states = std::collections::BTreeSet::from([0]);
    for c in input {
        let read = match c.to_ascii_lowercase() {
            '0' => 'o',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            other => other,
        };
        let mut next = std::collections::BTreeSet::from([0]);
        for position in states {
            if !c.is_ascii_alphabetic() {
                next.insert(position);
            }
            if letters.get(position) == Some(&read) {
                if position + 1 == letters.len() {
                    return true;
                }
                next.insert(position + 1);
            }
        }
        states = next;
    }
    false
}
pub(super) fn defang(value: &str, words: &[String]) -> String {
    let chars: Vec<_> = value.chars().collect();
    let openers = [
        0x3c, 0x2c2, 0x1438, 0x2039, 0x2329, 0x276c, 0x276e, 0x2770, 0x16b2, 0x2d66, 0x1032d,
        0x16aec, 0x1d236, 0x27e8, 0x27ea, 0x29fc, 0x3008, 0x300a, 0xfe64, 0xff1c,
    ];
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if !openers.contains(&(*c as u32)) {
                return *c;
            }
            let rest = &chars[i + 1..];
            let nearby = &rest[..rest.len().min(64)];
            let start = nearby
                .iter()
                .position(|c| !c.is_ascii_whitespace() && *c != '/')
                .unwrap_or(nearby.len());
            let end = nearby[start..]
                .iter()
                .position(|c| c.is_ascii_whitespace() || ['>', '\u{ff1e}'].contains(c))
                .map_or(nearby.len(), |n| n + start);
            let word = &nearby[start..end];
            if (end == nearby.len() && rest.len() > 64)
                || word.iter().any(|c| !c.is_ascii_graphic())
                || words.iter().any(|w| spells(word, w))
            {
                '['
            } else {
                *c
            }
        })
        .collect()
}
