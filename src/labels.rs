//! Text people are known by: participant names, nicknames, account display names
//! and room labels. Beyond length and control characters, each rejects Unicode
//! formatting that reorders or hides what is shown, and the name the interface
//! reserves for the reader.

use icu_casemap::CaseMapper;
use icu_normalizer::ComposingNormalizer;
use icu_properties::{CodePointSetData, props::DefaultIgnorableCodePoint};

#[cfg(test)]
#[path = "labels_property_tests.rs"]
mod property_tests;

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

/// Every Unicode default-ignorable character (bidi embeddings, overrides and
/// isolates, the zero-width space, word joiner and invisible operators, soft
/// hyphen, Hangul fillers, the byte-order mark, deprecated and unassigned
/// format characters) plus the line and paragraph separators, except what
/// displayed text needs: the joiners and direction marks of emoji sequences
/// and right-to-left scripts, variation selectors, Mongolian selectors and the
/// tag characters of subdivision flags.
fn is_hidden_format(character: char) -> bool {
    if matches!(character, '\u{2028}' | '\u{2029}') {
        return true;
    }
    let needed = matches!(
        character,
        '\u{200C}'..='\u{200F}'
            | '\u{061C}'
            | '\u{180B}'..='\u{180F}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{E0020}'..='\u{E007F}'
            | '\u{E0100}'..='\u{E01EF}'
    );
    !needed && CodePointSetData::new::<DefaultIgnorableCodePoint>().contains(character)
}

/// Message text keeps its line breaks and tabs; otherwise it is as plain as a
/// label: no other control character and nothing that reorders or hides what
/// is shown, so a pasted link reads the way it resolves.
pub fn is_plain_text(text: &str) -> bool {
    !text.chars().any(|character| {
        (character.is_control() && character != '\n' && character != '\t')
            || is_hidden_format(character)
    })
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
    use super::{comparison_key, is_plain, is_plain_text, is_reserved_name};

    #[test]
    fn plain_labels_keep_scripts_and_emoji_but_not_reordering_or_hidden_text() {
        for label in [
            "Maya",
            "Zoë",
            "李雷",
            "👩🏽\u{200D}💻 Sam",
            "\u{200F}عربي",
            "Jo\u{200C}n",
            "❤\u{FE0F} Sam",
            "🏴\u{E0067}\u{E0062}\u{E0073}\u{E0063}\u{E0074}\u{E007F} Sam",
            "\u{1820}\u{180B} Sam",
            "\u{061C}عربي",
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
            "S\u{2060}am",
            "S\u{00AD}am",
            "S\u{034F}am",
            "Sam\u{3164}",
            "Sam\u{115F}",
            "S\u{2062}am",
            "S\u{206A}am",
            "Sam\u{E0001}",
            "Sam\u{1D173}",
        ] {
            assert!(!is_plain(label), "{label:?}");
        }
    }

    #[test]
    fn message_text_keeps_line_breaks_but_not_hidden_formatting() {
        for text in [
            "hello\nworld\tdone",
            "",
            "👩🏽\u{200D}💻 \u{200F}عربي",
            "https://example.com/path?q=1",
        ] {
            assert!(is_plain_text(text), "{text:?}");
        }
        for text in [
            "moc.elpmaxe\u{202E}",
            "\u{2066}hidden\u{2069}",
            "ex\u{200B}ample.com",
            "ex\u{2060}ample.com",
            "ex\u{00AD}ample.com",
            "\u{FEFF}",
            "line\u{2028}break",
            "bell\u{0007}",
            "\r\n",
        ] {
            assert!(!is_plain_text(text), "{text:?}");
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
