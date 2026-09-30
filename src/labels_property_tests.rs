//! Finite, seeded properties of identity comparison; no external inputs or services.

use super::{comparison_key, is_reserved_name};
use icu_normalizer::DecomposingNormalizer;
use rand::{RngExt, SeedableRng, rngs::StdRng};

#[test]
fn boundary_properties_unicode_keys_are_idempotent_and_canonically_equivalent() {
    let mut rng = StdRng::seed_from_u64(0x006c_6162_656c_7301);
    let representative = [
        'A', 'ß', 'Σ', 'ς', 'Ｙ', 'K', 'ﬃ', 'é', '\u{0308}', '\u{0345}', '\u{200d}', '\u{200f}',
        '\u{fe0f}', ' ', '李', '🦀',
    ];
    let decomposer = DecomposingNormalizer::new_nfd();
    for case in 0..2048 {
        let mut text = String::new();
        for index in 0..case % 33 {
            text.push(if index % 2 == 0 {
                representative[rng.random_range(0..representative.len())]
            } else {
                rng.random::<char>()
            });
        }
        let key = comparison_key(&text);
        assert_eq!(comparison_key(&key), key, "idempotence, case {case}");
        assert_eq!(
            comparison_key(&decomposer.normalize(&text)),
            key,
            "canonical equivalence, case {case}"
        );
        assert_eq!(key.trim(), key, "stable boundary whitespace, case {case}");
    }
}

#[test]
fn boundary_properties_ascii_identity_is_stable_under_case_space_and_ignorables() {
    let mut rng = StdRng::seed_from_u64(0x006c_6162_656c_7302);
    let ignorables = ['\u{200c}', '\u{200d}', '\u{200f}', '\u{fe0f}', '\u{2060}'];
    for case in 0..1024 {
        let plain: String = if case == 0 {
            "you".into()
        } else {
            (0..rng.random_range(1..=32))
                .map(|_| char::from(b'a' + rng.random_range(0..26)))
                .collect()
        };
        let mut decorated = String::from("\u{2002} ");
        for character in plain.chars() {
            decorated.push(ignorables[rng.random_range(0..ignorables.len())]);
            decorated.push(if rng.random::<bool>() {
                character.to_ascii_uppercase()
            } else {
                character
            });
        }
        decorated.push_str(" \u{2002}");
        assert_eq!(comparison_key(&decorated), plain, "case {case}");
        assert_eq!(is_reserved_name(&decorated), plain == "you");
        assert_ne!(comparison_key(&(plain.clone() + "x")), plain);
    }
}
