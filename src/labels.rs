//! Text people are known by: participant names, nicknames, account display names
//! and room labels. Beyond length and control characters, each rejects Unicode
//! formatting that reorders or hides what is shown, and the name the interface
//! reserves for the reader.

use icu_casemap::CaseMapper;
use icu_normalizer::ComposingNormalizer;
use icu_properties::{CodePointSetData, props::DefaultIgnorableCodePoint};

/// Identity comparison only: preserve the original spelling for display.
/// Compatibility normalization, full Unicode case folding and removal of
/// default-ignorables prevent formatting from creating another identity with
/// the same visible name. This is not a cross-script confusable detector.
pub fn comparison_key(text: &str) -> String {
    let ignorables = CodePointSetData::new::<DefaultIgnorableCodePoint>();
    let visible: String = text.chars().filter(|c| !ignorables.contains(*c)).collect();
    let normalized = ComposingNormalizer::new_nfkc().normalize(visible.trim());
    let folded = CaseMapper::new().fold_string(&normalized);
    ComposingNormalizer::new_nfkc()
        .normalize(&folded)
        .trim()
        .to_owned()
}

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
    !comparison_key(text).is_empty()
        && !text
            .chars()
            .any(|character| character.is_control() || is_hidden_format(character))
}

/// Chat shows the reader's own messages as "You"; nobody else may claim it.
pub fn is_reserved_name(name: &str) -> bool {
    comparison_key(name) == "you"
}

#[cfg(test)]
mod tests {
    use super::{comparison_key, is_plain, is_reserved_name};

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
        for name in [
            "You",
            "you",
            "YOU",
            "  you ",
            "Y\u{200D}ou",
            "Y\u{200C}ou",
            "\u{200F}You",
            "Ｙｏｕ",
            "Yo\u{FE0F}u",
        ] {
            assert!(is_reserved_name(name), "{name:?}");
        }
        for name in ["Your", "You2", "Yo", "Youssef"] {
            assert!(!is_reserved_name(name), "{name:?}");
        }
    }

    #[test]
    fn comparison_covers_canonical_compatibility_case_and_ignorable_variants() {
        for (left, right) in [
            ("Zoë", "Zoe\u{308}"),
            ("Straße", "STRASSE"),
            ("Σ", "ς"),
            ("Ｍａｙａ", "maya"),
            ("Ma\u{200D}ya", "Maya"),
        ] {
            assert_eq!(comparison_key(left), comparison_key(right));
        }
        for invisible in ["\u{200D}", "\u{200F}\u{FE0F}", "  \u{200C}  "] {
            assert!(!is_plain(invisible));
        }
        assert_ne!(
            comparison_key("Maya"),
            comparison_key("Мауа"),
            "cross-script confusables require the displayed stable identity"
        );
    }
}
