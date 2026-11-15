//! The freshness of the pinned OpenSSL.
//!
//! A database of vulnerabilities can lag the vendor's own advisory, so the
//! pinned OpenSSL is also held to the vendor's list of current releases:
//! the newest stable release that the official page links must be the one
//! the native manifest pins. Nothing is upgraded here, and being current is
//! no proof that the native code is free of vulnerabilities.
//!
//! The page is HTML, and only the anchors a reader of HTML would see count.
//! The reader here takes the page as the reference's did wherever it takes
//! it at all, and refuses what it is not sure to read alike: a page that
//! hides a release from this reader could pass for current.

use crate::{
    Fault, Result,
    context::{Context, Outcome, Run},
    fast::Programs,
    files, hexadecimal,
    record::{self, pretty},
    require,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsStr,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Where the vendor lists its current releases.
const SOURCE: &str = "https://openssl-library.org/source/";
/// The most bytes of the page, and of the native manifest.
const MAX_PAGE: u64 = 1024 * 1024;
/// The most anchors of the page.
const MAX_LINKS: usize = 1000;
/// How long the fetch has, and the most bytes it may complain.
const FETCH: (Duration, u64) = (Duration::from_secs(35), 16384);
/// What an address of a release begins with, before its version.
const ARCHIVE: &str = "https://github.com/openssl/openssl/releases/download/openssl-";
/// The receipt, in the output directory.
const RECEIPT: &str = "openssl-freshness.json";
/// A page this reader does not take.
const FORM: Fault = Fault("openssl_release_page_form");
/// A native manifest that is no JSON, or holds a value of another kind.
const MANIFEST: Fault = Fault("openssl_native_manifest_format");

/// Whether `text` is a version: three numbers of at most four digits, none
/// with a nought before it.
fn version(text: &str) -> bool {
    let number = |part: &str| {
        let digits =
            !part.is_empty() && part.len() <= 4 && part.bytes().all(|b| b.is_ascii_digit());
        digits && (part == "0" || !part.starts_with('0'))
    };
    text.split('.').count() == 3 && text.split('.').all(number)
}

/// The version of the release whose archive `address` is, when it is one:
/// the vendor's own repository, the directory and the file of one version.
fn release(address: &str) -> Option<&str> {
    let (directory, file) = address.strip_prefix(ARCHIVE)?.split_once('/')?;
    let named = file.strip_prefix("openssl-")?.strip_suffix(".tar.gz")?;
    (version(directory) && named == directory).then_some(directory)
}

/// The three numbers of a version.
fn numbers(version: &str) -> Vec<u16> {
    let number = |part: &str| part.parse().unwrap_or(u16::MAX);
    version.split('.').map(number).collect()
}

/// The five characters that part a tag: no other space does.
fn space(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\n' | b'\r' | b'\x0c' | b' ')
}

/// One start tag: its name, its attributes in order, each with its value
/// or with none, whether it closes itself, and how many bytes it is.
struct Tag<'a> {
    name: &'a str,
    attributes: Vec<(&'a str, &'a str)>,
    closes: bool,
    length: usize,
}

/// Read the start tag that `rest` begins with. Outside quotes a tag is
/// printable ASCII and the five spaces. A name is letters, digits and
/// `:_-`; a value without quotes holds no quote, `<`, `=` or backtick; a
/// quote that opens also closes; and the tag ends before the page does.
fn tag(rest: &str) -> Result<Tag<'_>> {
    let bytes = rest.as_bytes();
    let parts = |byte: u8| space(byte) || matches!(byte, b'/' | b'>');
    let mut at = 1 + bytes[1..].iter().take_while(|byte| !parts(**byte)).count();
    let name = &rest[1..at];
    let lettered = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-');
    require(name.bytes().all(lettered), FORM.0)?;
    let mut attributes = Vec::new();
    loop {
        let between = |at: usize| match bytes.get(at) {
            Some(b'/') => bytes.get(at + 1) != Some(&b'>'),
            Some(byte) => space(*byte),
            None => false,
        };
        while between(at) {
            at += 1;
        }
        let closes = match bytes.get(at).ok_or(FORM)? {
            b'>' => Some(false),
            b'/' => Some(true),
            _ => None,
        };
        if let Some(closes) = closes {
            let length = at + 1 + usize::from(closes);
            return Ok(Tag {
                name,
                attributes,
                closes,
                length,
            });
        }
        let named = |byte: u8| !parts(byte) && byte != b'=';
        let end = at + bytes[at..].iter().take_while(|byte| named(**byte)).count();
        let plain =
            |byte: u8| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\'' | b'<' | b'`');
        let attribute = &rest[at..end];
        require(
            !attribute.is_empty() && attribute.bytes().all(plain),
            FORM.0,
        )?;
        let equals = end + bytes[end..].iter().take_while(|byte| space(**byte)).count();
        if bytes.get(equals) != Some(&b'=') {
            attributes.push((attribute, ""));
            at = end;
            continue;
        }
        let after = equals + 1;
        let begun = after
            + bytes[after..]
                .iter()
                .take_while(|byte| space(**byte))
                .count();
        let value = match bytes.get(begun) {
            Some(quote @ (b'"' | b'\'')) => {
                let length = rest[begun + 1..].find(char::from(*quote)).ok_or(FORM)?;
                at = begun + length + 2;
                &rest[begun + 1..begun + 1 + length]
            }
            _ => {
                let ended = |byte: u8| space(byte) || byte == b'>';
                at = begun
                    + bytes[begun..]
                        .iter()
                        .take_while(|byte| !ended(**byte))
                        .count();
                let value = &rest[begun..at];
                require(
                    value.bytes().all(|byte| plain(byte) && byte != b'='),
                    FORM.0,
                )?;
                value
            }
        };
        attributes.push((attribute, value));
    }
}

/// How many bytes the comment is that `rest` begins with. It ends at the
/// first `-->` or `--!>`, or at once with `>` or `->`.
fn comment(rest: &str) -> Result<usize> {
    let body = &rest[4..];
    if body.starts_with('>') || body.starts_with("->") {
        return Ok(4 + body.find('>').map_or(0, |at| at + 1));
    }
    let plain = body.find("-->").map(|at| (at, 3));
    let marked = body.find("--!>").map(|at| (at, 4));
    let (at, length) = [plain, marked].into_iter().flatten().min().ok_or(FORM)?;
    Ok(4 + at + length)
}

/// Whether the address `address` holds no reference to a character but the
/// ampersand's own. A reference is written with a number sign, or as a name
/// of at most 32 characters that a semicolon ends; what one would become is
/// the reference's to say, and a point, a colon or a letter can be written
/// so. An ampersand before anything else stands for itself, or for a
/// character no release's address holds.
fn literal(address: &str) -> bool {
    let named = |character: &char| {
        !matches!(
            character,
            '\t' | '\n' | '\x0c' | ' ' | '<' | '&' | '#' | ';'
        )
    };
    address.match_indices('&').all(|(at, _)| {
        let after = &address[at + 1..];
        let name: String = after.chars().take_while(named).take(32).collect();
        let ended = after[name.len()..].starts_with(';');
        !after.starts_with('#') && (!ended || name.is_empty() || name == "amp")
    })
}

/// How many bytes the declaration is that `rest` begins with. Only the
/// declaration of the document's type is one, and it holds no tag.
fn declaration(rest: &str) -> Result<usize> {
    let named = rest.as_bytes().get(2..9);
    let declared = named.is_some_and(|named| named.eq_ignore_ascii_case(b"doctype"));
    let end = rest.find('>').filter(|_| declared).ok_or(FORM)?;
    require(!rest[1..end].contains('<'), FORM.0)?;
    Ok(end + 1)
}

/// How many bytes the end tag is that `rest` begins with: its name, and
/// nothing but space before it ends.
fn closing(rest: &str) -> Result<usize> {
    let lettered = |byte: &u8| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-');
    let name = rest[2..].bytes().take_while(lettered).count();
    let spaces = rest[2 + name..]
        .bytes()
        .take_while(|byte| space(*byte))
        .count();
    let begun = rest[2..].starts_with(|first: char| first.is_ascii_alphabetic());
    let ended = rest.as_bytes().get(2 + name + spaces) == Some(&b'>');
    require(begun && ended, FORM.0)?;
    Ok(2 + name + spaces + 1)
}

/// What a page lists: how many anchors it holds, and the versions of the
/// releases they link, in the order of the page.
#[derive(Debug, Default, PartialEq, Eq)]
struct Listed {
    links: usize,
    versions: Vec<String>,
}

/// The elements whose content is text up to their own end tag.
const TEXT: [&str; 4] = ["script", "style", "title", "textarea"];
/// Elements with rules of their own for what follows them: a page that
/// holds one is not taken.
const REFUSED: [&str; 5] = ["iframe", "noembed", "noframes", "plaintext", "xmp"];

/// Where the content of the text element `name` ends in `rest`: before its
/// end tag, whose name is followed by a space or `>`.
fn ended(rest: &str, name: &str) -> Result<usize> {
    let ends = rest.match_indices("</").find(|(at, _)| {
        let after = &rest.as_bytes()[at + 2..];
        let named =
            after.len() > name.len() && after[..name.len()].eq_ignore_ascii_case(name.as_bytes());
        named && (space(after[name.len()]) || after[name.len()] == b'>')
    });
    Ok(ends.ok_or(FORM)?.0)
}

/// Read the page. Every anchor counts, and one whose only address is a
/// release's archive lists that release. A page with an anchor's address
/// that is not [`literal`] is not taken.
fn listed(page: &str) -> Result<Listed> {
    require(!page.contains('\0'), FORM.0)?;
    let mut found = Listed::default();
    let mut at = 0;
    while let Some(offset) = page[at..].find('<') {
        let rest = &page[at + offset..];
        let next = rest.as_bytes().get(1).copied();
        at += offset;
        if rest.starts_with("<!--") {
            at += comment(rest)?;
        } else if next == Some(b'!') {
            at += declaration(rest)?;
        } else if next == Some(b'/') {
            at += closing(rest)?;
        } else if next.is_some_and(|byte| byte.is_ascii_alphabetic()) {
            let tag = tag(rest)?;
            at += tag.length;
            let named = |name: &str| tag.name.eq_ignore_ascii_case(name);
            require(!REFUSED.iter().any(|name| named(name)), FORM.0)?;
            if tag.name.eq_ignore_ascii_case("a") {
                found.links += 1;
                require(found.links <= MAX_LINKS, "openssl_release_link_count")?;
                let addressed = |(name, _): &&(&str, &str)| name.eq_ignore_ascii_case("href");
                let addresses: Vec<_> = tag.attributes.iter().filter(addressed).collect();
                require(addresses.len() <= 1, "openssl_release_duplicate_href")?;
                if let Some((_, address)) = addresses.first() {
                    require(literal(address), FORM.0)?;
                    found.versions.extend(release(address).map(str::to_owned));
                }
            }
            if let Some(name) = TEXT.iter().find(|name| named(name)).filter(|_| !tag.closes) {
                at += ended(&page[at..], name)?;
            }
        } else if next == Some(b'?') {
            return Err(FORM);
        } else {
            at += 1;
        }
    }
    Ok(found)
}

/// The newest stable release the page lists. The page lists one release of
/// each series: none at all, or two of one series, is no list to go by.
fn latest(page: &[u8], pinned: &str) -> Result<String> {
    require(version(pinned), "openssl_pinned_version")?;
    require(
        !page.is_empty() && page.len() as u64 <= MAX_PAGE,
        "openssl_release_page_size",
    )?;
    let listed = listed(std::str::from_utf8(page).map_err(|_| FORM)?)?;
    let mut distinct: Vec<&str> = Vec::new();
    for version in &listed.versions {
        distinct.push(version.rsplit_once('.').map_or("", |(series, _)| series));
    }
    distinct.sort_unstable();
    distinct.dedup();
    require(
        !distinct.is_empty() && distinct.len() == listed.versions.len(),
        "openssl_release_series_missing_or_ambiguous",
    )?;
    let newest = listed
        .versions
        .iter()
        .max_by_key(|version| numbers(version));
    Ok(newest.cloned().unwrap_or_default())
}

/// Fetch the page from its one address: over HTTPS alone, without
/// redirects, credentials or a second try.
fn fetched(context: &Context, programs: &dyn Programs) -> Outcome<Vec<u8>> {
    let curl = programs.prerequisite("curl")?;
    let size = MAX_PAGE.to_string();
    let argv = [
        curl.as_os_str(),
        OsStr::new("--disable"),
        OsStr::new("--fail"),
        OsStr::new("--silent"),
        OsStr::new("--show-error"),
        OsStr::new("--proto"),
        OsStr::new("=https"),
        OsStr::new("--tlsv1.2"),
        OsStr::new("--connect-timeout"),
        OsStr::new("10"),
        OsStr::new("--max-time"),
        OsStr::new("30"),
        OsStr::new("--max-filesize"),
        OsStr::new(&size),
        OsStr::new("--header"),
        OsStr::new("Cache-Control: no-cache"),
        OsStr::new("--header"),
        OsStr::new("Accept: text/html"),
        OsStr::new("--write-out"),
        OsStr::new("\n%{http_code}"),
        OsStr::new(SOURCE),
    ];
    let how = Run {
        within: FETCH.0,
        ..Run::default()
    };
    let (status, mut page) = context.ask(&argv, &how, (MAX_PAGE + 4, FETCH.1))?;
    require(status == 0, "openssl_release_fetch_failed")?;
    require(page.ends_with(b"\n200"), "openssl_release_http_status")?;
    page.truncate(page.len() - 4);
    Ok(page)
}

/// `moment` as the receipt writes a time: the date and the time of day in
/// UTC, with the millionths of the second unless there are none.
fn written(moment: SystemTime) -> String {
    let since = moment.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (days, second) = (since.as_secs() / 86400, since.as_secs() % 86400);
    // The civil date of a count of days, by eras of four hundred years.
    let shifted = days + 719_468;
    let (era, day_of_era) = (shifted / 146_097, shifted % 146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    let millionths = match since.subsec_micros() {
        0 => String::new(),
        millionths => format!(".{millionths:06}"),
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{millionths}+00:00",
        second / 3600,
        second % 3600 / 60,
        second % 60,
    )
}

/// What the check learned, for the summary: the pinned version and the
/// newest that is listed.
struct Observed {
    pinned: String,
    latest: String,
}

/// Compare the pin of the snapshot's native manifest with the page, and
/// write the receipt of what was seen at `moment`, current or not.
fn observed(
    context: &Context,
    programs: &dyn Programs,
    snapshot: &Path,
    moment: SystemTime,
) -> Outcome<Observed> {
    let manifest = files::bounded(
        &snapshot.join("vendor/native-components.json"),
        MAX_PAGE,
        false,
    )?;
    let document = record::strict(&manifest).map_err(|_| MANIFEST)?;
    let component = document.get("openssl").and_then(Value::as_object);
    let component = component.ok_or(MANIFEST)?;
    let pinned = component.get("version").and_then(Value::as_str);
    let pinned = pinned.ok_or(MANIFEST)?;
    let source = component.get("source").and_then(Value::as_object);
    let address = source
        .and_then(|source| source.get("url"))
        .and_then(Value::as_str);
    let same = release(address.ok_or(MANIFEST)?) == Some(pinned);
    require(same, "openssl_source_version_mismatch")?;
    let page = fetched(context, programs)?;
    let latest = latest(&page, pinned)?;
    let receipt = json!({
        "schemaVersion": 1,
        "source": SOURCE,
        "observedAt": written(moment),
        "pageSha256": hexadecimal(&Sha256::digest(&page)),
        "nativeManifestSha256": hexadecimal(&Sha256::digest(&manifest)),
        "pinnedVersion": pinned,
        "latestVersion": latest,
        "passed": latest == pinned,
    });
    let receipt = pretty(&receipt);
    files::write_private(&context.output().join(RECEIPT), receipt.as_bytes(), 0o600)?;
    require(latest == pinned, "openssl_stable_update_required")?;
    Ok(Observed {
        pinned: pinned.to_owned(),
        latest,
    })
}

/// Require the pinned OpenSSL of the snapshot to be the vendor's newest
/// stable release, as seen at `moment`. The summary's line is begun before
/// anything is read and says how the check ended.
pub fn check(
    context: &mut Context,
    programs: &dyn Programs,
    snapshot: &Path,
    moment: SystemTime,
) -> Outcome<()> {
    let started = Instant::now();
    let line = context.begin(vec![
        ("name", "openssl-freshness".into()),
        ("exitStatus", 1.into()),
    ]);
    let outcome = observed(context, programs, snapshot, moment);
    if let Ok(observed) = &outcome {
        context.complete(
            line,
            [
                ("exitStatus", 0.into()),
                ("pinnedVersion", observed.pinned.as_str().into()),
                ("latestVersion", observed.latest.as_str().into()),
            ],
        );
    }
    let elapsed = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
    context.complete(line, [("elapsedSeconds", elapsed.into())]);
    outcome.map(|_| ())
}

#[cfg(test)]
mod tests;
