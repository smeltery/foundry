//! Display sanitization with explicit invitation prefixes, without decoding secrets.
use super::TicketCodec;

/// Render one line with at most 60 characters, including the truncation mark.
pub fn display_name(name: &str) -> String {
    let mut characters = name.chars().map(|c| {
        if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') {
            ' '
        } else {
            c
        }
    });
    let mut result: String = characters.by_ref().take(60).collect();
    if characters.next().is_some() {
        result.pop();
        result.push('…');
    }
    result
}

impl TicketCodec<'_> {
    /// Replace each whitespace-delimited word containing a secret-bearing ticket
    /// fragment. Preserve surrounding whitespace; this is not general secret detection.
    pub fn redact(&self, text: &str) -> String {
        let mut output = String::with_capacity(text.len());
        for piece in text.split_inclusive(char::is_whitespace) {
            let word = piece.trim_end_matches(char::is_whitespace);
            if has_secret(word, &self.profile.ticket_prefixes) {
                output.push_str("(team link)");
                output.push_str(&piece[word.len()..]);
            } else {
                output.push_str(piece);
            }
        }
        output
    }
}

fn has_secret(word: &str, prefixes: &[String]) -> bool {
    let mut previous = None;
    for (offset, character) in word.char_indices() {
        if unseen(character) {
            // An invisible separator breaks the preceding ASCII-letter boundary.
            previous = None;
            continue;
        }
        if previous.is_none_or(|c: char| !c.is_ascii_alphabetic()) {
            for prefix in prefixes {
                let mut visible = word[offset..].chars().filter(|c| !unseen(*c));
                if prefix.chars().all(|c| visible.next() == Some(c))
                    && visible
                        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                        .take(22)
                        .count()
                        == 22
                {
                    return true;
                }
            }
        }
        previous = Some(character);
    }
    false
}

// Default-ignorable ranges accepted by this display protocol, plus Markdown escapes.
fn unseen(c: char) -> bool {
    matches!(c, '\\' | '\u{00ad}' | '\u{034f}' | '\u{061c}' | '\u{115f}'..='\u{1160}'
        | '\u{17b4}'..='\u{17b5}' | '\u{180b}'..='\u{180f}' | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{3164}'
        | '\u{fe00}'..='\u{fe0f}' | '\u{feff}' | '\u{ffa0}' | '\u{1bca0}'..='\u{1bca3}'
        | '\u{e0000}'..='\u{e0fff}')
}
