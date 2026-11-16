//! The image's own policy, and what the scanners say of the image held to
//! it: the database the vulnerability scanner used, the inventory, and the
//! vulnerabilities it found. A scanner that ends well proves nothing by
//! that alone: an empty inventory, a stale database, an unknown scanner
//! release or a filter that hides findings is refused.

use super::{advisories, small_hex};
use crate::{
    Fault, Result,
    document::Document,
    files,
    policy::{Exception, permitted},
    require,
};
use std::{collections::BTreeSet, path::Path};

/// The bounds of the reports and of what they hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The most bytes of a scanner's report.
    pub report: u64,
    /// The most packages of the inventory.
    pub packages: usize,
    /// The most findings of the vulnerability scanner.
    pub matches: usize,
}

impl Limits {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        report: 64 * 1024 * 1024,
        packages: 20_000,
        matches: 100_000,
    };
}

/// The most bytes of the policy file.
const POLICY_BYTES: u64 = 65536;
/// The fewest and the most hours a policy may let a database age.
const DATABASE_HOURS: std::ops::RangeInclusive<i128> = 1..=120;
const FIELDS: [&str; 6] = [
    "scannerBase",
    "distribution",
    "maximumDatabaseAgeHours",
    "blockedSeverities",
    "allowedLicenses",
    "firstParty",
];
/// A report that is no JSON document.
const REPORT: Fault = Fault("image_report_invalid");
/// An inventory that lacks its packages or holds one that is no record.
const INVENTORY: Fault = Fault("image_inventory_shape");
/// A vulnerability report that lacks its findings or holds a malformed one.
const FINDINGS: Fault = Fault("image_vulnerability_report");

/// The scanner's report at `path`: a regular file of at most `limit`
/// bytes that holds one JSON document, no field of a record named twice.
pub fn report(path: &Path, limit: u64) -> Result<Document> {
    let body = files::bounded(path, limit, false)?;
    Document::read(&body).map_err(|_| REPORT)
}

/// The image's policy at `path`, every one of its fields held to what is
/// reviewed: a misspelt restriction is refused, never passed over.
pub fn load_policy(path: &Path) -> Result<Document> {
    let policy = report(path, POLICY_BYTES)?;
    let fields = policy.fields().ok_or(Fault("image_policy_fields"))?;
    let names: BTreeSet<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
    require(names == BTreeSet::from(FIELDS), "image_policy_fields")?;
    let field = |name: &str| policy.get(name).cloned().unwrap_or(Document::Null);
    let base = field("scannerBase");
    let base = base
        .text()
        .and_then(|base| base.strip_prefix("docker.io/library/fedora:44@sha256:"));
    require(
        base.is_some_and(|digest| small_hex(digest, 64)),
        "image_scanner_base",
    )?;
    let distribution = Document::record([("id", "fedora".into()), ("version", "44".into())]);
    require(
        field("distribution").sorted() == distribution,
        "image_os_policy",
    )?;
    let hours = field("maximumDatabaseAgeHours").whole();
    let hours = hours.is_some_and(|hours| DATABASE_HOURS.contains(&hours));
    require(hours, "image_database_age_policy")?;
    let severities = Document::List(vec!["High".into(), "Critical".into()]);
    require(
        field("blockedSeverities") == severities,
        "image_severity_policy",
    )?;
    let first_party = Document::record([
        ("cargoPublish", false.into()),
        ("license", "NOASSERTION".into()),
        ("name", "simplestchat".into()),
        ("policy", "unpublished-application".into()),
        ("source", "local".into()),
    ]);
    require(
        field("firstParty").sorted() == first_party,
        "image_first_party_policy",
    )?;
    let licenses = field("allowedLicenses");
    let licenses = licenses.items().unwrap_or_default();
    let named: Option<BTreeSet<&str>> = licenses.iter().map(Document::text).collect();
    let unique = named.is_some_and(|named| named.len() == licenses.len());
    require(!licenses.is_empty() && unique, "image_license_policy")?;
    Ok(policy)
}

/// A moment, in microseconds since the beginning of 1970 in UTC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Moment(pub i64);

impl Moment {
    /// What the clock says now.
    pub fn now() -> Self {
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
        let micros = since.map_or(0, |since| since.as_micros());
        Self(i64::try_from(micros).unwrap_or(i64::MAX))
    }
}

/// How many days lie between 1970-01-01 and the day `year-month-day`.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year - era * 400;
    let of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let of_era_days = of_era * 365 + of_era / 4 - of_era / 100 + of_year;
    era * 146_097 + of_era_days - 719_468
}

/// What a time of building is: a moment, or a day and time that does not
/// say of which place, or nothing that can be read.
enum Built {
    At(Moment),
    Naive,
}

/// A time as the scanner's database writes it: `YYYY-MM-DDTHH:MM:SS`, a
/// fraction of a second if there is one, and `Z` or an offset `±HH:MM`.
/// A day and time without an offset, or a day alone, says of no place.
fn built(text: &str) -> Option<Built> {
    let bytes = text.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = bytes.get(range)?;
        let digits = part.iter().all(u8::is_ascii_digit);
        digits
            .then(|| std::str::from_utf8(part).ok()?.parse().ok())
            .flatten()
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let dashed = bytes.get(4) == Some(&b'-') && bytes.get(7) == Some(&b'-');
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let length = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(dashed && year >= 1 && (1..=length).contains(&day)) {
        return None;
    }
    if bytes.len() == 10 {
        return Some(Built::Naive);
    }
    let timed = bytes.get(10) == Some(&b'T') && bytes.get(13) == Some(&b':');
    let (hour, minute) = (number(11..13)?, number(14..16)?);
    let second = number(17..19).filter(|_| bytes.get(16) == Some(&b':'))?;
    if !(timed && hour < 24 && minute < 60 && second < 60) {
        return None;
    }
    let mut at = 19;
    let mut micros = 0;
    if bytes.get(at) == Some(&b'.') {
        let digits = bytes[at + 1..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        // Beyond a microsecond nothing is kept; no digit is no number.
        let kept = &text[at + 1..at + 1 + digits.min(6)];
        micros = kept.parse::<i64>().ok()? * 10_i64.pow(6 - kept.len() as u32);
        at += 1 + digits;
    }
    let offset = match &text[at..] {
        "" => return Some(Built::Naive),
        "Z" => 0,
        zone => {
            let sign = match zone.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let zone = zone.as_bytes();
            let part = |range: std::ops::Range<usize>| -> Option<i64> {
                let part = zone.get(range)?;
                let digits = part.iter().all(u8::is_ascii_digit);
                digits
                    .then(|| std::str::from_utf8(part).ok()?.parse().ok())
                    .flatten()
            };
            let (hours, minutes) = (part(1..3)?, part(4..6)?);
            let shaped = zone.len() == 6 && zone[3] == b':' && hours < 24 && minutes < 60;
            if !shaped {
                return None;
            }
            sign * (hours * 60 + minutes) * 60
        }
    };
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    Some(Built::At(Moment((seconds - offset) * 1_000_000 + micros)))
}

/// Text of a query as the reference decodes it: a plus sign is a space,
/// every `%` and two hexadecimal digits the byte they spell, and what is no
/// UTF-8 a replacement character.
fn decoded(text: &str) -> String {
    let text = text.replace('+', " ");
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((first, after)) = rest.split_first() {
        // No plus sign is left to be taken for the sign of a number.
        let digits = after.get(..2).filter(|_| *first == b'%');
        let digits = digits.and_then(|digits| std::str::from_utf8(digits).ok());
        match digits.and_then(|digits| u8::from_str_radix(digits, 16).ok()) {
            Some(byte) => {
                bytes.push(byte);
                rest = &after[2..];
            }
            None => {
                bytes.push(*first);
                rest = after;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The values that the query of `address` gives the name `name`, as the
/// reference reads a query: fields without a sign or a value are passed
/// over.
fn query_values(query: &str, name: &str) -> Vec<String> {
    let fields = query.split('&').filter_map(|field| field.split_once('='));
    let fields = fields.filter(|(_, value)| !value.is_empty());
    let named = fields.filter(|(held, _)| decoded(held) == name);
    named.map(|(_, value)| decoded(value)).collect()
}

/// The status of the vulnerability scanner's database: valid, of the
/// sixth schema, fetched from the scanner's own site with a checksum, and
/// built no longer ago than the policy allows and not after `now`.
pub fn database_status(value: &Document, policy: &Document, now: Moment) -> Result<()> {
    let valid = value.get("valid") == Some(&Document::Bool(true));
    require(valid, "image_database_invalid")?;
    let schema = value.get("schemaVersion").and_then(Document::text);
    let schema = schema.and_then(|schema| schema.strip_prefix("v6."));
    let numbered = |part: &str| {
        (1..=3).contains(&part.len()) && part.bytes().all(|digit| digit.is_ascii_digit())
    };
    let schema = schema.and_then(|schema| schema.split_once('.'));
    let schema = schema.is_some_and(|(minor, patch)| numbered(minor) && numbered(patch));
    require(schema, "image_database_schema")?;
    // The address is read as it is written. The reference's URL library
    // dropped tabs, returns and line ends wherever they were and cut a
    // fragment off, so that two fields could become one: an address that
    // holds any of them is refused here, and whatever is taken here was
    // taken there.
    let source = value
        .get("from")
        .and_then(Document::text)
        .unwrap_or_default();
    let plain = !source.contains(['\t', '\r', '\n', '#']);
    let address = source
        .strip_prefix("https://grype.anchore.io/databases/v6/")
        .filter(|_| plain);
    let query = address
        .and_then(|address| address.split_once('?'))
        .map(|(_, query)| query);
    let checksum = query
        .map(|query| query_values(query, "checksum"))
        .unwrap_or_default();
    let checked = match checksum.as_slice() {
        [checksum] => checksum
            .strip_prefix("sha256:")
            .is_some_and(|digest| small_hex(digest, 64)),
        _ => false,
    };
    require(checked, "image_database_source")?;
    let built = value.get("built").and_then(Document::text).and_then(built);
    let built = match built.ok_or(Fault("image_database_time"))? {
        Built::At(moment) => moment,
        Built::Naive => return Err(Fault("image_database_timezone")),
    };
    let hours = policy
        .get("maximumDatabaseAgeHours")
        .and_then(Document::whole);
    let most = hours.map(|hours| hours * 3600 * 1_000_000);
    let age = i128::from(now.0) - i128::from(built.0);
    require(
        most.is_some_and(|most| (0..=most).contains(&age)),
        "image_database_stale",
    )
}

/// The text field `name` of a package, when it is not empty.
fn words<'a>(item: &'a Document, name: &str) -> Option<&'a str> {
    item.get(name)
        .and_then(Document::text)
        .filter(|text| !text.is_empty())
}

/// The packages of the inventory `value`, made by the scanner release
/// `syft` of the policy's distribution: at least one rpm and the
/// application's own crate among them, each with a name, a version and an
/// identity of its own.
pub fn inventory(
    value: &Document,
    policy: &Document,
    syft: &str,
    limits: &Limits,
) -> Result<Vec<Document>> {
    let distribution = policy.get("distribution").ok_or(Fault("image_os_policy"))?;
    let distro = |name: &str| value.get("distro").and_then(|distro| distro.get(name));
    let same = distro("id") == distribution.get("id")
        && distro("versionID") == distribution.get("version");
    require(same, "image_distribution_unsupported")?;
    let descriptor = value.get("descriptor");
    let named = descriptor.and_then(|descriptor| descriptor.get("name")) == Some(&"syft".into());
    let released =
        descriptor.and_then(|descriptor| descriptor.get("version")) == Some(&syft.into());
    require(named && released, "image_sbom_tool_identity")?;
    let packages = value
        .get("artifacts")
        .and_then(Document::items)
        .ok_or(INVENTORY)?;
    require(
        packages.iter().all(|item| item.fields().is_some()),
        INVENTORY.0,
    )?;
    require(
        (1..=limits.packages).contains(&packages.len()),
        "image_inventory_empty",
    )?;
    let kind = |item: &Document, kind: &str| item.get("type") == Some(&kind.into());
    require(
        packages.iter().any(|item| kind(item, "rpm")),
        "image_rpm_inventory_missing",
    )?;
    let application = |item: &&Document| {
        kind(item, "rust-crate") && item.get("name") == Some(&"simplestchat".into())
    };
    require(
        packages.iter().any(|item| application(&item)),
        "image_rust_inventory_missing",
    )?;
    let whole = packages.iter().all(|item| {
        words(item, "name").is_some()
            && words(item, "version").is_some()
            && words(item, "id").is_some()
    });
    require(whole, "image_package_identity")?;
    let identities: BTreeSet<&str> = packages
        .iter()
        .filter_map(|item| words(item, "id"))
        .collect();
    require(
        identities.len() == packages.len(),
        "image_package_duplicate",
    )?;
    Ok(packages.to_vec())
}

/// What the vulnerability report `value` of the scanner release `grype`
/// says of the image: every finding of a high, critical or unknown
/// severity is blocked unless a review waives it for its exact package,
/// and the reviewed advisory at `advisory` blocks the affected packages of
/// OpenSSL among `packages`, which no review of the scanner's waives.
pub fn vulnerability_verdict(
    value: &Document,
    exceptions: &[Exception],
    packages: &[Document],
    grype: &str,
    advisory: &Path,
    limits: &Limits,
) -> Result<Document> {
    let descriptor = value.get("descriptor");
    let named = descriptor.and_then(|descriptor| descriptor.get("name")) == Some(&"grype".into());
    let released =
        descriptor.and_then(|descriptor| descriptor.get("version")) == Some(&grype.into());
    require(named && released, "image_vulnerability_tool_identity")?;
    let ignored = value
        .get("ignoredMatches")
        .map_or(Some(&[][..]), Document::items);
    let ignored = ignored.ok_or(Fault("image_ignored_vulnerabilities"))?;
    require(ignored.is_empty(), "image_ignored_vulnerabilities")?;
    let matches = value
        .get("matches")
        .and_then(Document::items)
        .ok_or(FINDINGS)?;
    require(matches.len() <= limits.matches, "image_vulnerability_count")?;
    let (mut blocked, mut waived) = (Vec::new(), Vec::new());
    for item in matches {
        let record = |name: &str| item.get(name).filter(|value| value.fields().is_some());
        let (vulnerability, package) = (record("vulnerability"), record("artifact"));
        let (Some(vulnerability), Some(package)) = (vulnerability, package) else {
            return Err(FINDINGS);
        };
        let severity = vulnerability
            .get("severity")
            .and_then(Document::text)
            .ok_or(FINDINGS)?;
        let known = ["Negligible", "Low", "Medium", "High", "Critical", "Unknown"];
        require(known.contains(&severity), "image_vulnerability_severity")?;
        if !matches!(severity, "High" | "Critical" | "Unknown") {
            continue;
        }
        let scope = package
            .get("purl")
            .and_then(Document::text)
            .ok_or(FINDINGS)?;
        let fingerprint = vulnerability
            .get("id")
            .and_then(Document::text)
            .ok_or(FINDINGS)?;
        let fix = vulnerability
            .get("fix")
            .filter(|fix| fix.fields().is_some())
            .ok_or(FINDINGS)?;
        let finding = Document::record([
            ("id", fingerprint.into()),
            ("scope", scope.into()),
            ("severity", severity.into()),
            (
                "fixState",
                fix.get("state").cloned().unwrap_or(Document::Null),
            ),
        ]);
        if permitted(exceptions, "grype", fingerprint, scope) {
            waived.push(finding);
        } else {
            blocked.push(finding);
        }
    }
    let advisories = advisories::verdict(packages, advisory, &advisories::Limits::REVIEWED)?;
    let reviewed = advisories
        .get("blocked")
        .and_then(Document::items)
        .unwrap_or_default();
    blocked.extend_from_slice(reviewed);
    Ok(Document::record([
        ("passed", blocked.is_empty().into()),
        ("matches", matches.len().into()),
        ("blocked", Document::List(blocked)),
        ("waived", Document::List(waived)),
        ("reviewedAdvisories", advisories),
    ]))
}

/// Whether the vulnerability report `value` was made with the `database`
/// whose status was checked, of the three providers that the image needs,
/// and without a filter that hides findings.
pub fn vulnerability_database_binding(value: &Document, database: &Document) -> Result<()> {
    const CHANGED: Fault = Fault("image_vulnerability_database_changed");
    let descriptor = value.get("descriptor");
    let db = descriptor.and_then(|descriptor| descriptor.get("db"));
    let status = |key: &str| {
        db.and_then(|db| db.get("status"))
            .and_then(|status| status.get(key))
    };
    for key in ["schemaVersion", "from", "built", "valid"] {
        let checked = database.get(key).ok_or(CHANGED)?;
        require(status(key) == Some(checked), CHANGED.0)?;
    }
    let providers = db
        .and_then(|db| db.get("providers"))
        .and_then(Document::fields);
    let providers = providers.ok_or(Fault("image_vulnerability_providers_missing"))?;
    let provided = |name: &str| providers.iter().any(|(held, _)| held == name);
    let complete = ["fedora", "github", "nvd"].into_iter().all(provided);
    require(complete, "image_vulnerability_providers_missing")?;
    let configuration = descriptor.and_then(|descriptor| descriptor.get("configuration"));
    let set = |name: &str| configuration.and_then(|configuration| configuration.get(name));
    let unfiltered = set("only-fixed") == Some(&false.into())
        && set("only-notfixed") == Some(&false.into())
        && set("ignore-wontfix") == Some(&"".into());
    require(unfiltered, "image_vulnerability_filter")
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
