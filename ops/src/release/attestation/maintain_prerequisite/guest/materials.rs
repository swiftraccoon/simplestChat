//! Passive validation of exactly seven retained natural Caddy material byte strings.
//! Filesystem custody and endpoint authority stay with the fixture owner.
//!
//! [`inspect`] holds the materials to a reference analyzer: its checks in its order,
//! its clock arithmetic and its closed [`Refusal`] vocabulary. `recorded_materials.json` holds
//! that reference's verdicts over this module's fixture variants. [`analyze`] reports a refusal
//! in the guest's own failure categories.

use super::{Error, Result};
use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use openssl::{asn1::Asn1Object, nid::Nid, pkey::PKey, x509::X509};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt,
    ops::RangeInclusive,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use x509_parser::{
    der_parser::der::{Class, DerObject},
    extensions::{GeneralName, GeneralSubtree, ParsedExtension},
    prelude::parse_x509_certificate,
};

pub(super) const NAME: &str = "maintain.localhost";
pub(super) const PATHS: [&str; 7] = [
    "pki/authorities/local/root.crt",
    "pki/authorities/local/root.key",
    "pki/authorities/local/intermediate.crt",
    "pki/authorities/local/intermediate.key",
    "certificates/local/maintain.localhost/maintain.localhost.crt",
    "certificates/local/maintain.localhost/maintain.localhost.key",
    "certificates/local/maintain.localhost/maintain.localhost.json",
];
const KNOWN: [&str; 7] = [
    "2.5.29.19",
    "2.5.29.15",
    "2.5.29.37",
    "2.5.29.17",
    "2.5.29.14",
    "2.5.29.35",
    "1.3.6.1.5.5.7.1.1",
];
const UNSUPPORTED: [&str; 5] = [
    "2.5.29.30",
    "2.5.29.36",
    "2.5.29.54",
    "2.5.29.32",
    "2.5.29.33",
];
const KEY_LABELS: [&str; 3] = ["PRIVATE KEY", "EC PRIVATE KEY", "RSA PRIVATE KEY"];
/// Bytes of one material, of the metadata document and of all seven together.
const MAX_MATERIAL: usize = 65_536;
const MAX_METADATA: usize = 16_384;
const MAX_TOTAL: usize = 458_752;
/// The reference's calendar in microseconds since the epoch: 0001-01-01T00:00:00Z through
/// 9999-12-31T23:59:59.999999Z. A clock or a fixture end outside it cannot be held.
const CALENDAR: RangeInclusive<i128> = -62_135_596_800_000_000..=253_402_300_799_999_999;
const MICROS: i128 = 1_000_000;
/// The fractions of a lifetime after which renewal begins, as the exact ratios of the binary64
/// values `2 / 3` (leaf) and `0.8` (authorities) the reference multiplies by.
const LEAF_RENEWAL: (i128, i128) = (6_004_799_503_160_661, 9_007_199_254_740_992);
const AUTHORITY_RENEWAL: (i128, i128) = (3_602_879_701_896_397, 4_503_599_627_370_496);
/// A leaf must outlive the fixture end by more than fifty minutes.
const LEAF_MARGIN: i128 = 50 * 60 * MICROS;
/// The longest integer literal the reference's JSON reader converts.
const MAX_INTEGER_DIGITS: usize = 4300;

/// The reference analyzer's closed refusal vocabulary; each is shown as its fixed public code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The interval is empty, or the clock or the fixture end lies outside the calendar.
    FixtureClock,
    /// The reference's own library prerequisite. This analyzer's libraries are fixed when it
    /// is built, so it never reports this code.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the reference's code, which this analyzer never reports"
        )
    )]
    AnalyzerPrerequisite,
    MaterialSet,
    MaterialSize,
    MetadataJson,
    MetadataDuplicate,
    MetadataSchema,
    CertificatePem,
    CertificateChain,
    CertificateStructure,
    CertificateExtension,
    CertificateOcsp,
    PrivateKeyPem,
    PrivateKeyMismatch,
    AuthorityConstraints,
    AuthorityPathLength,
    LeafConstraints,
    LeafSubject,
    CertificateSignature,
    CertificateValidity,
    CertificateRenewalWindow,
}
type Refused<T> = std::result::Result<T, Refusal>;
impl Refusal {
    pub(super) const fn code(self) -> &'static str {
        match self {
            Self::FixtureClock => "fixture_clock",
            Self::AnalyzerPrerequisite => "analyzer_prerequisite",
            Self::MaterialSet => "material_set",
            Self::MaterialSize => "material_size",
            Self::MetadataJson => "metadata_json",
            Self::MetadataDuplicate => "metadata_duplicate",
            Self::MetadataSchema => "metadata_schema",
            Self::CertificatePem => "certificate_pem",
            Self::CertificateChain => "certificate_chain",
            Self::CertificateStructure => "certificate_structure",
            Self::CertificateExtension => "certificate_extension",
            Self::CertificateOcsp => "certificate_ocsp",
            Self::PrivateKeyPem => "private_key_pem",
            Self::PrivateKeyMismatch => "private_key_mismatch",
            Self::AuthorityConstraints => "authority_constraints",
            Self::AuthorityPathLength => "authority_path_length",
            Self::LeafConstraints => "leaf_constraints",
            Self::LeafSubject => "leaf_subject",
            Self::CertificateSignature => "certificate_signature",
            Self::CertificateValidity => "certificate_validity",
            Self::CertificateRenewalWindow => "certificate_renewal_window",
        }
    }
    /// The guest failure category a refusal is reported in, the one the guest reported before
    /// it had these codes: the bounds are `bounds`; the clock, the set, the prerequisite and
    /// unreadable JSON are `input`; every certificate refusal is `refused`. The guest read
    /// the metadata as a record of exactly `sans`, an array of strings, and `issuer_data`, any
    /// value: a document it could not read so is `input`, other values are `refused`. One
    /// reference code spans both, so a repeated key or a schema refusal is told apart by that
    /// reading.
    fn error(self, metadata: &[u8]) -> Error {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Shaped {
            #[serde(rename = "sans")]
            _sans: Vec<String>,
            #[serde(rename = "issuer_data")]
            _issuer_data: Value,
        }
        match self {
            Self::MaterialSize => Error::Bounds,
            Self::FixtureClock
            | Self::AnalyzerPrerequisite
            | Self::MaterialSet
            | Self::MetadataJson => Error::Input,
            Self::MetadataDuplicate | Self::MetadataSchema => {
                if serde_json::from_slice::<Shaped>(metadata).is_ok() {
                    Error::Refused
                } else {
                    Error::Input
                }
            }
            _ => Error::Refused,
        }
    }
}
impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.code())
    }
}

pub(super) struct Materials {
    certificates: [CertificateFacts; 3],
    chain_count: usize,
    /// Complete DERs in the actual required verified order: leaf, intermediate, root.
    pub der: [Vec<u8>; 3],
    pub der_hashes: [String; 3],
}
struct CertificateFacts {
    certificate_sha256: String,
    public_key_sha256: String,
    not_before: DateTime<Utc>,
    not_after: DateTime<Utc>,
    lifetime_micros: i128,
    remaining_micros: i128,
    renewal_margin_micros: i128,
}
impl CertificateFacts {
    fn public(&self) -> Value {
        json!({"certificateSha256":self.certificate_sha256,"publicKeySha256":self.public_key_sha256,
            "notBefore":self.not_before.to_rfc3339_opts(SecondsFormat::Secs,false),
            "notAfter":self.not_after.to_rfc3339_opts(SecondsFormat::Secs,false),
            "lifetimeSeconds":self.lifetime_micros as f64/1_000_000.0,
            "remainingSeconds":self.remaining_micros as f64/1_000_000.0,
            "renewalMarginAfterFixtureSeconds":self.renewal_margin_micros as f64/1_000_000.0})
    }
}
impl Materials {
    pub fn facts(&self) -> Value {
        json!({"schemaVersion":1,"name":NAME,"analysis":"retained-materials-only",
            "analyzer":"rust-openssl-x509-parser","opensslVersion":openssl::version::version(),
            "chainCertificateCount":self.chain_count,"metadataSansCount":1,
            "metadataIssuerDataNull":true,"ocspUriCount":0,
            "root":self.certificates[0].public(),
            "intermediate":self.certificates[1].public(),"leaf":self.certificates[2].public()})
    }
}

/// The clock in microseconds, rounded down as a UTC timestamp is, and the fixture end: the
/// interval is rounded to the nearest microsecond, ties to even, as the reference rounds it.
fn clock(now: SystemTime, remaining: Duration) -> Refused<(i128, i128)> {
    if remaining.is_zero() {
        return Err(Refusal::FixtureClock);
    }
    let now = match now.duration_since(UNIX_EPOCH) {
        Ok(after) => i128::try_from(after.as_micros()),
        Err(before) => i128::try_from(before.duration().as_nanos().div_ceil(1000)).map(|v| -v),
    }
    .map_err(|_| Refusal::FixtureClock)?;
    let nanos = remaining.as_nanos();
    let (whole, part) = (nanos / 1000, nanos % 1000);
    let rounded = whole + u128::from(part > 500 || (part == 500 && whole % 2 == 1));
    let end = i128::try_from(rounded)
        .ok()
        .and_then(|micros| now.checked_add(micros))
        .ok_or(Refusal::FixtureClock)?;
    if !CALENDAR.contains(&now) || !CALENDAR.contains(&end) {
        return Err(Refusal::FixtureClock);
    }
    Ok((now, end))
}

/// What the metadata check needs of one JSON value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Null,
    /// An array holding exactly the fixed name.
    Name,
    Other,
}
/// An object's members in order: each key as code points, and its value's shape.
type Members = Vec<(Vec<u32>, Shape)>;
/// One value the reader has completed.
enum Read {
    Null,
    /// A string, and whether it is the fixed name.
    Text(bool),
    /// An array, and whether it holds exactly the fixed name.
    Array(bool),
    Object(Members),
    Other,
}
impl Read {
    fn shape(&self) -> Shape {
        match self {
            Self::Null => Shape::Null,
            Self::Array(true) => Shape::Name,
            _ => Shape::Other,
        }
    }
}
/// One container the reader has opened and not yet closed.
enum Open {
    /// Elements so far, and whether the first is the fixed name.
    Array(usize, bool),
    /// Members so far, and the key awaiting its value.
    Object(Members, Vec<u32>),
}

/// The reference's strict JSON reader. Whitespace is space, tab, line feed and carriage
/// return. `NaN` and the infinities are refused; an integer literal of more than 4300 digits
/// is refused, a float literal of any size is read. A `\u` escape may name a lone surrogate.
/// An object that repeats a key is refused when it closes. Nesting is bounded only by the
/// document's own bound.
struct Reader {
    text: Vec<char>,
    at: usize,
}
impl Reader {
    fn peek(&self) -> Option<char> {
        self.text.get(self.at).copied()
    }
    fn space(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.at += 1;
        }
    }
    fn take(&mut self, wanted: char) -> bool {
        let found = self.peek() == Some(wanted);
        if found {
            self.at += 1;
        }
        found
    }
    fn word(&mut self, word: &str) -> bool {
        let found = word
            .chars()
            .enumerate()
            .all(|(offset, wanted)| self.text.get(self.at + offset) == Some(&wanted));
        if found {
            self.at += word.chars().count();
        }
        found
    }
    fn digits(&mut self) -> usize {
        let start = self.at;
        while matches!(self.peek(), Some('0'..='9')) {
            self.at += 1;
        }
        self.at - start
    }
    fn hex(&mut self) -> Refused<u32> {
        let mut unit = 0;
        for _ in 0..4 {
            let digit = self
                .peek()
                .and_then(|value| value.to_digit(16))
                .ok_or(Refusal::MetadataJson)?;
            self.at += 1;
            unit = unit * 16 + digit;
        }
        Ok(unit)
    }
    /// The rest of a string whose opening quote was taken, as code points.
    fn string(&mut self) -> Refused<Vec<u32>> {
        let mut text = Vec::new();
        loop {
            let value = self.peek().ok_or(Refusal::MetadataJson)?;
            self.at += 1;
            match value {
                '"' => return Ok(text),
                '\\' => {
                    let escape = self.peek().ok_or(Refusal::MetadataJson)?;
                    self.at += 1;
                    text.push(match escape {
                        '"' | '\\' | '/' => u32::from(escape),
                        'b' => 8,
                        'f' => 12,
                        'n' => 10,
                        'r' => 13,
                        't' => 9,
                        'u' => {
                            let high = self.hex()?;
                            let mark = self.at;
                            if (0xd800..=0xdbff).contains(&high) && self.word("\\u") {
                                let low = self.hex()?;
                                if (0xdc00..=0xdfff).contains(&low) {
                                    0x10000 + (((high - 0xd800) << 10) | (low - 0xdc00))
                                } else {
                                    self.at = mark;
                                    high
                                }
                            } else {
                                high
                            }
                        }
                        _ => return Err(Refusal::MetadataJson),
                    });
                }
                value if u32::from(value) < 0x20 => return Err(Refusal::MetadataJson),
                value => text.push(u32::from(value)),
            }
        }
    }
    fn number(&mut self) -> Refused<()> {
        self.take('-');
        let integer = match self.peek() {
            Some('0') => {
                self.at += 1;
                1
            }
            Some('1'..='9') => self.digits(),
            _ => return Err(Refusal::MetadataJson),
        };
        let mut float = false;
        if self.peek() == Some('.') && matches!(self.text.get(self.at + 1), Some('0'..='9')) {
            self.at += 1;
            self.digits();
            float = true;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let mark = self.at;
            self.at += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                self.at = mark;
            } else {
                float = true;
            }
        }
        if !float && integer > MAX_INTEGER_DIGITS {
            return Err(Refusal::MetadataJson);
        }
        Ok(())
    }
    fn member(&mut self) -> Refused<Vec<u32>> {
        if !self.take('"') {
            return Err(Refusal::MetadataJson);
        }
        let key = self.string()?;
        self.space();
        if !self.take(':') {
            return Err(Refusal::MetadataJson);
        }
        Ok(key)
    }
    /// The whole document: the members of its top-level value when that is an object.
    fn document(mut self) -> Refused<Option<Members>> {
        let named = |text: &[u32]| text.iter().copied().eq(NAME.chars().map(u32::from));
        let mut open = Vec::new();
        'value: loop {
            self.space();
            let mut read = match self.peek() {
                Some('{') => {
                    self.at += 1;
                    self.space();
                    if !self.take('}') {
                        open.push(Open::Object(Vec::new(), self.member()?));
                        continue 'value;
                    }
                    Read::Object(Vec::new())
                }
                Some('[') => {
                    self.at += 1;
                    self.space();
                    if !self.take(']') {
                        open.push(Open::Array(0, false));
                        continue 'value;
                    }
                    Read::Array(false)
                }
                Some('"') => {
                    self.at += 1;
                    Read::Text(named(&self.string()?))
                }
                Some('n') if self.word("null") => Read::Null,
                Some('t') if self.word("true") => Read::Other,
                Some('f') if self.word("false") => Read::Other,
                Some('-' | '0'..='9') => {
                    self.number()?;
                    Read::Other
                }
                _ => return Err(Refusal::MetadataJson),
            };
            loop {
                self.space();
                match open.last_mut() {
                    None => {
                        return if self.at == self.text.len() {
                            Ok(match read {
                                Read::Object(members) => Some(members),
                                _ => None,
                            })
                        } else {
                            Err(Refusal::MetadataJson)
                        };
                    }
                    Some(Open::Array(count, first)) => {
                        if *count == 0 {
                            *first = matches!(read, Read::Text(true));
                        }
                        *count += 1;
                        if self.take(',') {
                            continue 'value;
                        }
                        if !self.take(']') {
                            return Err(Refusal::MetadataJson);
                        }
                        read = Read::Array(*count == 1 && *first);
                    }
                    Some(Open::Object(members, key)) => {
                        members.push((std::mem::take(key), read.shape()));
                        if self.take(',') {
                            self.space();
                            *key = self.member()?;
                            continue 'value;
                        }
                        if !self.take('}') {
                            return Err(Refusal::MetadataJson);
                        }
                        let members = std::mem::take(members);
                        let mut keys = BTreeSet::new();
                        if !members.iter().all(|(key, _)| keys.insert(key.as_slice())) {
                            return Err(Refusal::MetadataDuplicate);
                        }
                        read = Read::Object(members);
                    }
                }
                open.pop();
            }
        }
    }
}

/// The fixed record: exactly the keys `sans` and `issuer_data`, the fixed name alone in `sans`
/// and a null `issuer_data`.
fn metadata(raw: &[u8]) -> Refused<()> {
    let text = std::str::from_utf8(raw).map_err(|_| Refusal::MetadataJson)?;
    let reader = Reader {
        text: text.chars().collect(),
        at: 0,
    };
    let members = reader.document()?.ok_or(Refusal::MetadataSchema)?;
    let shape = |name: &str| {
        members
            .iter()
            .find(|(key, _)| key.iter().copied().eq(name.chars().map(u32::from)))
            .map(|(_, shape)| *shape)
    };
    if members.len() != 2
        || shape("sans") != Some(Shape::Name)
        || shape("issuer_data") != Some(Shape::Null)
    {
        return Err(Refusal::MetadataSchema);
    }
    Ok(())
}

fn whitespace(value: u8) -> bool {
    matches!(value, b' ' | b'\t' | b'\r' | b'\n' | 11 | 12)
}

/// Framing is deliberately strict before calling a mature PEM decoder: blocks of one of
/// `labels` whose bodies hold only base64 and line ends, with only whitespace around them.
fn frames<'a>(raw: &'a [u8], labels: &[&str]) -> Option<Vec<&'a [u8]>> {
    let mut at = 0;
    let mut result = Vec::new();
    while at < raw.len() {
        while at < raw.len() && whitespace(raw[at]) {
            at += 1;
        }
        if at == raw.len() {
            break;
        }
        let start = at;
        let label = labels
            .iter()
            .find(|label| raw[at..].starts_with(format!("-----BEGIN {label}-----").as_bytes()))?;
        at += format!("-----BEGIN {label}-----").len();
        let end = format!("-----END {label}-----");
        let count = raw[at..]
            .windows(end.len())
            .position(|v| v == end.as_bytes())?;
        if count == 0
            || !raw[at..at + count]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=\r\n".contains(b))
        {
            return None;
        }
        at += count + end.len();
        result.push(&raw[start..at]);
    }
    Some(result)
}

/// Every certificate of one PEM file; a file that frames or decodes none is refused.
fn certificate_set(raw: &[u8]) -> Refused<Vec<X509>> {
    let frames = frames(raw, &["CERTIFICATE"])
        .filter(|frames| !frames.is_empty())
        .ok_or(Refusal::CertificatePem)?;
    frames
        .into_iter()
        .map(|frame| {
            let (rest, pem) =
                x509_parser::pem::parse_x509_pem(frame).map_err(|_| Refusal::CertificatePem)?;
            if !rest.iter().all(|b| whitespace(*b)) || pem.label != "CERTIFICATE" {
                return Err(Refusal::CertificatePem);
            }
            let (rest, _) =
                parse_x509_certificate(&pem.contents).map_err(|_| Refusal::CertificatePem)?;
            if !rest.is_empty() {
                return Err(Refusal::CertificatePem);
            }
            X509::from_der(&pem.contents).map_err(|_| Refusal::CertificatePem)
        })
        .collect()
}
/// The TLS probe's view of one PEM file: at most three certificates, within the material bound.
pub(super) fn certificates(raw: &[u8]) -> Result<Vec<X509>> {
    if raw.is_empty() || raw.len() > MAX_MATERIAL {
        return Err(Error::Bounds);
    }
    let certificates = certificate_set(raw).map_err(|_| Error::Refused)?;
    if certificates.len() > 3 {
        return Err(Error::Refused);
    }
    Ok(certificates)
}
/// Exactly one framed key block, decoded without the whitespace around it: OpenSSL's reader
/// would miss a block that a vertical tab or a form feed precedes on its line.
fn key_matches(certificate: &X509, raw: &[u8]) -> Refused<()> {
    let [frame] = frames(raw, &KEY_LABELS)
        .and_then(|frames| <[_; 1]>::try_from(frames).ok())
        .ok_or(Refusal::PrivateKeyPem)?;
    let private = PKey::private_key_from_pem(frame).map_err(|_| Refusal::PrivateKeyPem)?;
    let actual = private
        .public_key_to_der()
        .map_err(|_| Refusal::PrivateKeyPem)?;
    let expected = certificate
        .public_key()
        .and_then(|key| key.public_key_to_der())
        .map_err(|_| Refusal::CertificateStructure)?;
    if actual != expected {
        return Err(Refusal::PrivateKeyMismatch);
    }
    Ok(())
}

fn complete_fields(oid: &str, object: &DerObject<'_>) -> Refused<()> {
    use Refusal::CertificateStructure as Structure;
    match oid {
        // These two x509-parser field parsers return an inner remainder that
        // der-parser's container intentionally discards. The mature DER tree
        // preserves every child, so require the complete fixed field shapes.
        "1.3.6.1.5.5.7.1.1" | "1.3.6.1.5.5.7.1.11" => {
            for description in object.as_sequence().map_err(|_| Structure)? {
                let fields = description.as_sequence().map_err(|_| Structure)?;
                if fields.len() != 2
                    || fields[0].as_oid().is_err()
                    || fields[1].class() != Class::ContextSpecific
                    || fields[1].tag().0 > 8
                {
                    return Err(Structure);
                }
            }
        }
        "2.5.29.35" => {
            let mut previous = None;
            for field in object.as_sequence().map_err(|_| Structure)? {
                let tag = field.tag().0;
                if field.class() != Class::ContextSpecific
                    || tag > 2
                    || previous.is_some_and(|previous| previous >= tag)
                {
                    return Err(Structure);
                }
                previous = Some(tag);
                if tag == 1 {
                    use x509_parser::asn1_rs::FromDer;
                    let mut bytes = field.as_slice().map_err(|_| Structure)?;
                    while !bytes.is_empty() {
                        let (rest, name) = GeneralName::from_der(bytes).map_err(|_| Structure)?;
                        if rest.len() >= bytes.len() {
                            return Err(Structure);
                        }
                        general_name(&name)?;
                        bytes = rest;
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn general_name(name: &GeneralName<'_>) -> Refused<()> {
    use Refusal::CertificateStructure as Structure;
    match name {
        GeneralName::Invalid(..) | GeneralName::X400Address(_) | GeneralName::EDIPartyName(_) => {
            Err(Structure)
        }
        GeneralName::IPAddress(bytes) if ![4, 16].contains(&bytes.len()) => Err(Structure),
        GeneralName::RegisteredID(oid) => oid_value(oid.as_bytes()),
        GeneralName::OtherName(oid, bytes) => {
            oid_value(oid.as_bytes())?;
            let (rest, explicit) =
                x509_parser::der_parser::der::parse_der(bytes).map_err(|_| Structure)?;
            if !rest.is_empty()
                || explicit.class() != Class::ContextSpecific
                || explicit.tag().0 != 0
                || !explicit.header.is_constructed()
            {
                return Err(Structure);
            }
            let (rest, _) = x509_parser::der_parser::der::parse_der(
                explicit.as_slice().map_err(|_| Structure)?,
            )
            .map_err(|_| Structure)?;
            if rest.is_empty() {
                Ok(())
            } else {
                Err(Structure)
            }
        }
        _ => Ok(()),
    }
}

fn oid_value(bytes: &[u8]) -> Refused<()> {
    use asn1_rs::Oid;
    use x509_parser::num_bigint::BigUint;
    // Decode the first combined subidentifier with the mature relative iterator:
    // the absolute iterator assumes it fits one byte. The direct optional
    // asn1-rs bigint feature preserves the full arc range without hidden feature
    // unification or a fixed integer ceiling.
    let relative = Oid::new_relative(Cow::Borrowed(bytes));
    let mut arcs = relative.iter_bigint();
    let combined = arcs.next().ok_or(Refusal::CertificateStructure)?;
    let (first, second) = if combined < BigUint::from(40_u8) {
        (0, combined)
    } else if combined < BigUint::from(80_u8) {
        (1, combined - BigUint::from(40_u8))
    } else {
        (2, combined - BigUint::from(80_u8))
    };
    let mut numeric = format!("{first}.{second}");
    for arc in arcs {
        numeric.push('.');
        numeric.push_str(&arc.to_string());
    }
    let native = Asn1Object::from_str(&numeric).map_err(|_| Refusal::CertificateStructure)?;
    if native.as_slice() == bytes {
        Ok(())
    } else {
        Err(Refusal::CertificateStructure)
    }
}

/// What the reference reads from one certificate's extensions, and its validity in seconds.
struct Profile {
    /// Basic constraints: a CA, and its path length bound.
    basic: Option<(bool, Option<u32>)>,
    /// Key usage: digital signature, and certificate signing.
    usage: Option<(bool, bool)>,
    /// Extended key usage: whether it names server authentication.
    server: Option<bool>,
    /// Subject alternative names: whether they are exactly the fixed DNS name.
    named: Option<bool>,
    dates: (i64, i64),
}

/// Whether the reference's certificate library can hold a decoded value: a path length only
/// for a CA, encipher- or decipher-only only with key agreement, an authority certificate's
/// issuer and serial number together, name constraints with a subtree, every subtree list
/// non-empty and no wildcard DNS subtree, and policy constraints with a constraint.
fn holdable(extension: &ParsedExtension<'_>) -> bool {
    match extension {
        ParsedExtension::BasicConstraints(value) => value.ca || value.path_len_constraint.is_none(),
        ParsedExtension::KeyUsage(value) => {
            value.key_agreement() || !(value.encipher_only() || value.decipher_only())
        }
        ParsedExtension::AuthorityKeyIdentifier(value) => {
            value.authority_cert_issuer.is_some() == value.authority_cert_serial.is_some()
        }
        ParsedExtension::NameConstraints(value) => {
            let wildcard = |subtree: &GeneralSubtree<'_>| match subtree.base {
                GeneralName::DNSName(name) => name.contains('*'),
                _ => false,
            };
            let trees = [&value.permitted_subtrees, &value.excluded_subtrees];
            trees.iter().any(|tree| tree.is_some())
                && trees.iter().all(|tree| {
                    tree.as_ref().is_none_or(|subtrees| {
                        !subtrees.is_empty() && !subtrees.iter().any(wildcard)
                    })
                })
        }
        ParsedExtension::PolicyConstraints(value) => {
            value.require_explicit_policy.is_some() || value.inhibit_policy_mapping.is_some()
        }
        _ => true,
    }
}

/// Every extension is read before any is judged: a repeated or unreadable extension refuses
/// the structure, then an unsupported or unknown critical extension, then an OCSP responder.
fn extensions(certificate: &X509) -> Refused<Profile> {
    use Refusal::CertificateStructure as Structure;
    let der = certificate.to_der().map_err(|_| Structure)?;
    let (rest, parsed) = parse_x509_certificate(&der).map_err(|_| Structure)?;
    if !rest.is_empty() {
        return Err(Structure);
    }
    let mut profile = Profile {
        basic: None,
        usage: None,
        server: None,
        named: None,
        dates: (
            parsed.validity().not_before.timestamp(),
            parsed.validity().not_after.timestamp(),
        ),
    };
    let mut seen = BTreeSet::new();
    let (mut unsupported, mut ocsp) = (false, false);
    for extension in parsed.extensions() {
        let oid = extension.oid.to_id_string();
        if !seen.insert(oid.clone()) {
            return Err(Structure);
        }
        unsupported |= UNSUPPORTED.contains(&oid.as_str())
            || (extension.critical && !KNOWN.contains(&oid.as_str()));
        if !matches!(
            extension.parsed_extension(),
            ParsedExtension::UnsupportedExtension { .. }
        ) {
            // x509-parser's extension dispatcher discards each inner parser's
            // remainder. Independently require one complete mature DER value.
            let (rest, object) =
                x509_parser::der_parser::der::parse_der(extension.value).map_err(|_| Structure)?;
            if !rest.is_empty() {
                return Err(Structure);
            }
            complete_fields(&oid, &object)?;
        } else if KNOWN.contains(&oid.as_str()) {
            return Err(Structure);
        }
        if !holdable(extension.parsed_extension()) {
            return Err(Structure);
        }
        match extension.parsed_extension() {
            ParsedExtension::ParseError { .. } | ParsedExtension::Unparsed => {
                return Err(Structure);
            }
            ParsedExtension::BasicConstraints(value) => {
                profile.basic = Some((value.ca, value.path_len_constraint));
            }
            ParsedExtension::KeyUsage(value) => {
                profile.usage = Some((value.digital_signature(), value.key_cert_sign()));
            }
            ParsedExtension::ExtendedKeyUsage(value) => profile.server = Some(value.server_auth),
            ParsedExtension::SubjectAlternativeName(value) => {
                for name in &value.general_names {
                    general_name(name)?;
                }
                profile.named = Some(matches!(
                    value.general_names.as_slice(),
                    [GeneralName::DNSName(name)] if *name == NAME
                ));
            }
            ParsedExtension::IssuerAlternativeName(value) => {
                for name in &value.general_names {
                    general_name(name)?;
                }
            }
            ParsedExtension::AuthorityInfoAccess(value) => {
                // The safe native decoder independently consumes the complete
                // AIA schema; presence is already established by this extension.
                certificate.authority_info().ok_or(Structure)?;
                for description in &value.accessdescs {
                    oid_value(description.access_method.as_bytes())?;
                    ocsp |= description.access_method.to_id_string() == "1.3.6.1.5.5.7.48.1";
                    general_name(&description.access_location)?;
                }
            }
            ParsedExtension::SubjectInfoAccess(value) => {
                for description in &value.accessdescs {
                    oid_value(description.access_method.as_bytes())?;
                    general_name(&description.access_location)?;
                }
            }
            // SKI/AKI and every other supported payload were parsed above. The frozen
            // helper does not invent an additional key-identifier linkage requirement.
            _ => {}
        }
    }
    if unsupported {
        return Err(Refusal::CertificateExtension);
    }
    if ocsp {
        return Err(Refusal::CertificateOcsp);
    }
    Ok(profile)
}
fn authority(profile: &Profile, root: bool) -> Refused<()> {
    let (Some((ca, path)), Some((_, signs))) = (profile.basic, profile.usage) else {
        return Err(Refusal::AuthorityConstraints);
    };
    if !ca || !signs {
        return Err(Refusal::AuthorityConstraints);
    }
    if root && path.is_some_and(|n| n < 1) {
        return Err(Refusal::AuthorityPathLength);
    }
    Ok(())
}
fn subject(certificate: &X509, profile: &Profile) -> Refused<()> {
    let (Some(named), Some((signature, signs)), Some(server)) =
        (profile.named, profile.usage, profile.server)
    else {
        return Err(Refusal::LeafConstraints);
    };
    if !named {
        return Err(Refusal::LeafSubject);
    }
    // Native full-length DirectoryString conversion also supports BMP/T61.
    // The deprecated as_utf8 truncates interior NULs in this pinned crate.
    let common = certificate
        .subject_name()
        .entries_by_nid(Nid::COMMONNAME)
        .map(|entry| entry.data().to_string())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Refusal::CertificateStructure)?;
    if !common.is_empty() && (common.len() != 1 || &*common[0] != NAME) {
        return Err(Refusal::LeafSubject);
    }
    if profile.basic.is_some_and(|(ca, _)| ca) || !signature || signs || !server {
        return Err(Refusal::LeafConstraints);
    }
    Ok(())
}
/// The issuer's subject names the certificate's issuer byte for byte, as the reference
/// compares them; a canonical comparison would let case and spacing differ.
fn issued(certificate: &X509, issuer: &X509) -> Refused<()> {
    use Refusal::CertificateSignature as Signature;
    if certificate.issuer_name().to_der().map_err(|_| Signature)?
        != issuer.subject_name().to_der().map_err(|_| Signature)?
    {
        return Err(Signature);
    }
    let public = issuer.public_key().map_err(|_| Signature)?;
    if !certificate.verify(&public).map_err(|_| Signature)? {
        return Err(Signature);
    }
    Ok(())
}
/// `lifetime` times one renewal fraction, rounded to the nearest microsecond, ties to even.
fn scaled(lifetime: i128, (numerator, denominator): (i128, i128)) -> i128 {
    let product = lifetime * numerator;
    let (quotient, remainder) = (product / denominator, product % denominator);
    match (2 * remainder).cmp(&denominator) {
        Ordering::Less => quotient,
        Ordering::Greater => quotient + 1,
        Ordering::Equal => quotient + quotient % 2,
    }
}
fn facts(
    certificate: &X509,
    dates: (i64, i64),
    now: i128,
    end: i128,
    leaf: bool,
) -> Refused<CertificateFacts> {
    let utc = |seconds| {
        DateTime::<Utc>::from_timestamp(seconds, 0)
            .filter(|v| (1..=9999).contains(&v.year()))
            .ok_or(Refusal::CertificateStructure)
    };
    let (not_before, not_after) = (utc(dates.0)?, utc(dates.1)?);
    let before = i128::from(dates.0) * MICROS;
    let after = i128::from(dates.1) * MICROS;
    if !(before <= now && now < end && end < after) {
        return Err(Refusal::CertificateValidity);
    }
    // Both dates lie within the calendar, so the product stays far inside the integer range.
    let lifetime = after - before;
    let renewal = before
        + scaled(
            lifetime,
            if leaf {
                LEAF_RENEWAL
            } else {
                AUTHORITY_RENEWAL
            },
        );
    if end >= renewal || (leaf && after - end <= LEAF_MARGIN) {
        return Err(Refusal::CertificateRenewalWindow);
    }
    let public = certificate
        .public_key()
        .and_then(|key| key.public_key_to_der())
        .map_err(|_| Refusal::CertificateStructure)?;
    Ok(CertificateFacts {
        certificate_sha256: super::render::digest(
            &certificate
                .to_der()
                .map_err(|_| Refusal::CertificateStructure)?,
        ),
        public_key_sha256: super::render::digest(&public),
        not_before,
        not_after,
        lifetime_micros: lifetime,
        remaining_micros: after - now,
        renewal_margin_micros: renewal - end,
    })
}

/// Inspect bytes against the sampled UTC clock and the original remaining interval, refusing
/// at the reference's first failing check with its code.
pub(super) fn inspect(
    materials: &BTreeMap<String, Vec<u8>>,
    now: SystemTime,
    remaining: Duration,
) -> Refused<Materials> {
    let (now, end) = clock(now, remaining)?;
    if materials.len() != PATHS.len() || PATHS.iter().any(|p| !materials.contains_key(*p)) {
        return Err(Refusal::MaterialSet);
    }
    if materials
        .values()
        .any(|v| v.is_empty() || v.len() > MAX_MATERIAL)
        || materials[PATHS[6]].len() > MAX_METADATA
        || materials.values().map(Vec::len).sum::<usize>() > MAX_TOTAL
    {
        return Err(Refusal::MaterialSize);
    }
    metadata(&materials[PATHS[6]])?;
    let roots = certificate_set(&materials[PATHS[0]])?;
    let intermediates = certificate_set(&materials[PATHS[2]])?;
    let chain = certificate_set(&materials[PATHS[4]])?;
    if roots.len() != 1 || intermediates.len() != 1 || ![2, 3].contains(&chain.len()) {
        return Err(Refusal::CertificateChain);
    }
    let [root]: [_; 1] = roots.try_into().map_err(|_| Refusal::CertificateChain)?;
    let [intermediate]: [_; 1] = intermediates
        .try_into()
        .map_err(|_| Refusal::CertificateChain)?;
    let leaf = &chain[0];
    let der = [leaf.to_der(), intermediate.to_der(), root.to_der()]
        .map(|v| v.map_err(|_| Refusal::CertificateStructure));
    let [leaf_der, intermediate_der, root_der] = der;
    let der = [leaf_der?, intermediate_der?, root_der?];
    if chain
        .iter()
        .map(|certificate| certificate.to_der())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Refusal::CertificateStructure)?
        != der[..chain.len()]
        || der[0] == der[1]
        || der[0] == der[2]
        || der[1] == der[2]
    {
        return Err(Refusal::CertificateChain);
    }
    let root_profile = extensions(&root)?;
    key_matches(&root, &materials[PATHS[1]])?;
    let intermediate_profile = extensions(&intermediate)?;
    key_matches(&intermediate, &materials[PATHS[3]])?;
    let leaf_profile = extensions(leaf)?;
    key_matches(leaf, &materials[PATHS[5]])?;
    authority(&root_profile, true)?;
    authority(&intermediate_profile, false)?;
    subject(leaf, &leaf_profile)?;
    issued(&root, &root)?;
    issued(&intermediate, &root)?;
    issued(leaf, &intermediate)?;
    let certificates = [
        facts(&root, root_profile.dates, now, end, false)?,
        facts(&intermediate, intermediate_profile.dates, now, end, false)?,
        facts(leaf, leaf_profile.dates, now, end, true)?,
    ];
    let der_hashes = der.each_ref().map(|v| super::render::digest(v));
    Ok(Materials {
        certificates,
        chain_count: chain.len(),
        der,
        der_hashes,
    })
}

/// Inspect bytes as [`inspect`] does, reporting a refusal in the guest's failure category.
pub(super) fn analyze(
    materials: &BTreeMap<String, Vec<u8>>,
    now: SystemTime,
    remaining: Duration,
) -> Result<Materials> {
    let result = inspect(materials, now, remaining)
        .map_err(|refusal| refusal.error(materials.get(PATHS[6]).map_or(&[][..], Vec::as_slice)))?;
    // Material facts retain the native analyzer identity and bounded private
    // representation; they never claim Python/cryptography execution.
    if serde_json::to_vec(&result.facts())
        .map_err(|_| Error::Input)?
        .len()
        > 16384
    {
        return Err(Error::Bounds);
    }
    Ok(result)
}

#[cfg(test)]
#[path = "materials_tests.rs"]
pub(super) mod tests;
