//! Text people are known by: participant names, nicknames, account display names
//! and room labels. Beyond length and control characters, each rejects Unicode
//! formatting that reorders or hides what is shown, and the name the interface
//! reserves for the reader.

/// Bidirectional embeddings, overrides and isolates, the zero-width space, the
/// byte-order mark and the line and paragraph separators. Joiners and direction
/// marks stay allowed: emoji sequences and right-to-left scripts need them.
fn is_hidden_format(character: char) -> bool {
    matches!(
        character,
        '\u{200B}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}' | '\u{FEFF}'
    )
}

/// A label that reads as it is: no control characters and no hidden formatting.
pub fn is_plain(text: &str) -> bool {
    !text
        .chars()
        .any(|character| character.is_control() || is_hidden_format(character))
}

/// Chat shows the reader's own messages as "You"; nobody else may claim it.
pub fn is_reserved_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("you")
}

#[cfg(test)]
mod tests {
    use super::{is_plain, is_reserved_name};

    #[test]
    fn plain_labels_keep_scripts_and_emoji_but_not_reordering_or_hidden_text() {
        for label in [
            "Maya",
            "Zoë",
            "李雷",
            "👩🏽\u{200D}💻 Sam",
            "\u{200F}عربي",
            "Jo\u{200C}n",
        ] {
            assert!(is_plain(label), "{label:?}");
        }
        for label in [
            "Ma\u{202E}ya",
            "\u{2066}Sam\u{2069}",
            "S\u{200B}am",
            "\u{FEFF}Sam",
            "Sam\u{2028}",
            "Sam\u{0007}",
        ] {
            assert!(!is_plain(label), "{label:?}");
        }
    }

    #[test]
    fn the_readers_own_name_is_reserved_in_any_case_and_spacing() {
        for name in ["You", "you", "YOU", "  you "] {
            assert!(is_reserved_name(name), "{name:?}");
        }
        for name in ["Your", "You2", "Yo", "Youssef"] {
            assert!(!is_reserved_name(name), "{name:?}");
        }
    }
}
