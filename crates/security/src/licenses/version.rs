//! A version of a Python package in its one spelling.
//!
//! A lock pins a version as it is written, and the name of each artifact
//! writes it again in its own way: with or without leading zeros, with
//! `alpha` for `a`, with another separator. Two spellings name one version
//! when their normal forms are equal, so a version is read into its normal
//! form or it is no version.

/// The labels of a pre-release, each before any label it starts, beside
/// the letters each is counted as.
const PRE: [(&str, &str); 8] = [
    ("alpha", "a"),
    ("a", "a"),
    ("beta", "b"),
    ("b", "b"),
    ("preview", "rc"),
    ("pre", "rc"),
    ("c", "rc"),
    ("rc", "rc"),
];
/// The labels of a post-release, likewise ordered.
const POST: [(&str, &str); 3] = [("post", ".post"), ("rev", ".post"), ("r", ".post")];
/// The label of a development release.
const DEV: [(&str, &str); 1] = [("dev", ".dev")];
const SEPARATORS: [char; 3] = ['.', '_', '-'];

/// How many leading bytes of `text` are digits.
fn digits(text: &str) -> usize {
    text.bytes().take_while(u8::is_ascii_digit).count()
}

/// A number as it is counted: without its leading zeros, and zero when
/// there is no digit at all.
fn counted(digits: &str) -> &str {
    match digits.trim_start_matches('0') {
        "" => "0",
        significant => significant,
    }
}

/// What follows `word` at the start of `text`, whatever the case of the
/// word's letters there.
fn after<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let start = text.get(..word.len())?;
    start
        .eq_ignore_ascii_case(word)
        .then(|| &text[word.len()..])
}

/// A labelled part at the start of `text`: at most one separator, one of
/// `labels`, at most one separator and a number that may be missing. The
/// part is added to `spelled` as its label is counted, and what follows it
/// is the answer; a text that starts with no such part is answered whole.
fn labelled<'a>(text: &'a str, labels: &[(&str, &str)], spelled: &mut String) -> &'a str {
    let named = text.strip_prefix(SEPARATORS).unwrap_or(text);
    let found = labels
        .iter()
        .find_map(|(label, counted)| Some((*counted, after(named, label)?)));
    let Some((label, numbered)) = found else {
        return text;
    };
    let numbered = numbered.strip_prefix(SEPARATORS).unwrap_or(numbered);
    let (number, rest) = numbered.split_at(digits(numbered));
    spelled.push_str(label);
    spelled.push_str(counted(number));
    rest
}

/// The normal form of the version `text`, or nothing when it is none: an
/// epoch that is not zero, the release, then the pre-release, post-release,
/// development and local parts that are there, each in its one spelling.
pub(crate) fn normal(text: &str) -> Option<String> {
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let mut spelled = String::new();
    let epoch = digits(text);
    let mut rest = match text[epoch..].strip_prefix('!') {
        Some(release) if epoch > 0 => {
            let epoch = counted(&text[..epoch]);
            if epoch != "0" {
                spelled.push_str(epoch);
                spelled.push('!');
            }
            release
        }
        _ => text,
    };
    loop {
        let (number, after) = rest.split_at(digits(rest));
        if number.is_empty() {
            return None;
        }
        spelled.push_str(counted(number));
        match after.strip_prefix('.') {
            Some(next) if digits(next) > 0 => {
                spelled.push('.');
                rest = next;
            }
            _ => {
                rest = after;
                break;
            }
        }
    }
    rest = labelled(rest, &PRE, &mut spelled);
    // A hyphen and a number alone are a post-release too.
    let implicit = rest.strip_prefix('-').filter(|number| digits(number) > 0);
    if let Some(numbered) = implicit {
        let (number, after) = numbered.split_at(digits(numbered));
        spelled.push_str(".post");
        spelled.push_str(counted(number));
        rest = after;
    } else {
        rest = labelled(rest, &POST, &mut spelled);
    }
    rest = labelled(rest, &DEV, &mut spelled);
    let Some(local) = rest.strip_prefix('+') else {
        return rest.is_empty().then_some(spelled);
    };
    spelled.push('+');
    for (index, segment) in local.split(SEPARATORS).enumerate() {
        let plain = segment.bytes().all(|byte| byte.is_ascii_alphanumeric());
        if segment.is_empty() || !plain {
            return None;
        }
        if index > 0 {
            spelled.push('.');
        }
        if digits(segment) == segment.len() {
            spelled.push_str(counted(segment));
        } else {
            spelled.push_str(&segment.to_ascii_lowercase());
        }
    }
    Some(spelled)
}
