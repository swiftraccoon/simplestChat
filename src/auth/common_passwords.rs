//! Offline password-selection refusal list: a pinned common-credential corpus
//! plus curated entries. Matching never sends credentials outside the process.

use std::{collections::HashSet, sync::OnceLock};

const CORPUS: &str = include_str!("../../vendor/seclists-passwords/10k-most-common.txt");
const MAX_AFFIX: usize = 16;
static KEYS: OnceLock<HashSet<&'static str>> = OnceLock::new();

const COMMON: &[&str] = &[
    "password",
    "password1",
    "password12",
    "password123",
    "password1234",
    "passw0rd",
    "p@ssw0rd",
    "p@ssword",
    "pa55word",
    "passwort",
    "12345678",
    "123456789",
    "1234567890",
    "123456789a",
    "1234567890a",
    "12345678a",
    "123456780",
    "1234561234",
    "11111111",
    "111111111",
    "00000000",
    "88888888",
    "12341234",
    "123123123",
    "1231231234",
    "987654321",
    "0987654321",
    "1234qwer",
    "1q2w3e4r",
    "1q2w3e4r5t",
    "1qaz2wsx",
    "1qaz2wsx3edc",
    "qwertyui",
    "qwertyuiop",
    "qwerty12",
    "qwerty123",
    "qwerty1234",
    "qwerty12345",
    "qwertyqwerty",
    "asdfghjk",
    "asdfghjkl",
    "asdfasdf",
    "asdf1234",
    "zxcvbnm1",
    "zxcvbnm123",
    "qazwsxedc",
    "qazwsx123",
    "abcdefgh",
    "abcd1234",
    "abc12345",
    "abc123456",
    "abcdef123",
    "a1b2c3d4",
    "aaaaaaaa",
    "iloveyou",
    "iloveyou1",
    "iloveyou2",
    "ilovemyself",
    "sunshine",
    "sunshine1",
    "princess",
    "princess1",
    "football",
    "football1",
    "baseball",
    "baseball1",
    "basketball",
    "superman",
    "superman1",
    "batman123",
    "spiderman",
    "trustno1",
    "welcome1",
    "welcome12",
    "welcome123",
    "welcome2024",
    "admin123",
    "admin1234",
    "adminadmin",
    "administrator",
    "letmein1",
    "letmein12",
    "letmein123",
    "whatever",
    "whatever1",
    "starwars",
    "starwars1",
    "computer",
    "computer1",
    "internet",
    "michael1",
    "michelle",
    "jennifer",
    "jessica1",
    "jordan23",
    "charlie1",
    "anthony1",
    "nicholas",
    "matthew1",
    "daniel123",
    "hello123",
    "hello1234",
    "goodbye1",
    "monkey12",
    "monkey123",
    "dragon12",
    "dragon123",
    "shadow12",
    "shadow123",
    "master12",
    "master123",
    "killer12",
    "killer123",
    "mustang1",
    "freedom1",
    "cheese123",
    "changeme",
    "changeme1",
    "changeme123",
    "secret123",
    "access14",
    "samsung1",
    "pokemon1",
    "pokemon123",
    "liverpool",
    "liverpool1",
    "chelsea1",
    "arsenal1",
    "chocolate",
    "butterfly",
    "corvette",
    "snoopy12",
    "flower123",
    "summer2024",
    "winter2024",
    "spring2024",
    "autumn2024",
    "letmeinnow",
    "openme123",
    "test1234",
    "testtest",
    "testing123",
    "temp1234",
    "temppass",
    "guest123",
    "guestguest",
    "user1234",
    "default1",
    "rootroot",
    "root1234",
    "toor1234",
    "system123",
    "server123",
    "login123",
    "loginlogin",
    "simplestchat",
    "chatchat",
    "chat1234",
    "research",
    "research1",
    "clinic123",
];

/// Refuse a known complete password or one common stem padded at either end
/// with a bounded number of ASCII digits/punctuation. This is selection only:
/// the actual password remains NFC-normalized, not case-folded or stripped.
pub fn is_common(password: &str) -> bool {
    let keys = KEYS.get_or_init(|| CORPUS.lines().chain(COMMON.iter().copied()).collect());
    let comparison = crate::labels::comparison_key(password);
    if keys.contains(comparison.as_str()) {
        return true;
    }
    let removable = |c: char| c.is_ascii_digit() || c.is_ascii_punctuation();
    let start = comparison
        .chars()
        .take(MAX_AFFIX)
        .take_while(|c| removable(*c))
        .count();
    let tail = &comparison[start..]; // Every removed character is one ASCII byte.
    let suffix = tail
        .chars()
        .rev()
        .take(MAX_AFFIX)
        .take_while(|c| removable(*c))
        .count();
    let core = &tail[..tail.len() - suffix];
    !core.is_empty() && keys.contains(core)
}

#[cfg(test)]
mod tests {
    use super::{COMMON, CORPUS, is_common};
    use sha2::{Digest, Sha256};

    #[test]
    fn vendored_corpus_and_license_match_the_reviewed_revision() {
        assert_eq!(
            hex::encode(Sha256::digest(CORPUS.as_bytes())),
            "68782d6a4a19a4768d5f15dd66bd534e7a33055cc755411e33f16d18c50fdcce"
        );
        assert_eq!(
            hex::encode(Sha256::digest(include_bytes!(
                "../../vendor/seclists-passwords/LICENSE"
            ))),
            "3dbdc93d5f8829de0941744841730a09c106d0732e5ae0e98ca1d77be7ded66c"
        );
        let entries: Vec<_> = CORPUS.lines().collect();
        assert_eq!(entries.len(), 10_001);
        assert!(entries.iter().all(|entry| !entry.is_empty()
            && entry.is_ascii()
            && *entry == entry.to_ascii_lowercase()));
        assert_eq!(
            entries
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            entries.len()
        );
    }

    #[test]
    fn common_stems_cannot_be_padded_with_years_symbols_or_invisible_characters() {
        for value in [
            "monkey20262026!!!",
            "!!Samantha20262026!",
            "Ｍｏｎｋｅｙ２０２６２０２６!!!",
            "mon\u{200D}key20262026!!!",
        ] {
            assert!(value.chars().count() >= 15);
            assert!(is_common(value), "{value:?}");
        }
        for value in [
            "the river carries moonlight gently",
            "correct horse battery staple",
            "Q7v_X9q-F4m@L2p~K8w-C6r!",
            "monkey joins five distant orchestras",
        ] {
            assert!(
                !is_common(value),
                "whole phrases are not reduced to individual words"
            );
        }
    }

    #[test]
    fn curated_entries_are_normalized_and_nonempty() {
        for entry in COMMON {
            assert_eq!(*entry, entry.to_lowercase(), "{entry}");
            assert!(!entry.is_empty());
        }
    }

    #[test]
    fn common_passwords_are_refused_in_any_case_and_uncommon_ones_pass() {
        assert!(is_common("Password123"));
        assert!(is_common("  QWERTYUIOP "));
        assert!(!is_common("correct horse battery staple"));
        assert!(!is_common("Zoë-rides-bikes-2026"));
    }
}
