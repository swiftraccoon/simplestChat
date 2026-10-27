//! Passive validation of exactly seven retained natural Caddy material byte strings.
//! Filesystem custody and endpoint authority stay with the fixture owner.

use super::{Error, Result};
use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use openssl::{asn1::Asn1Object, pkey::PKey, x509::X509};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use x509_parser::{
    der_parser::der::{Class, DerObject},
    extensions::{GeneralName, ParsedExtension},
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

fn whitespace(value: u8) -> bool {
    matches!(value, b' ' | b'\t' | b'\r' | b'\n' | 11 | 12)
}

/// Framing is deliberately strict before calling a mature PEM decoder.
fn frames<'a>(raw: &'a [u8], labels: &[&str]) -> Result<Vec<&'a [u8]>> {
    if raw.is_empty() || raw.len() > 65536 {
        return Err(Error::Bounds);
    }
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
            .find(|label| raw[at..].starts_with(format!("-----BEGIN {label}-----").as_bytes()))
            .ok_or(Error::Refused)?;
        at += format!("-----BEGIN {label}-----").len();
        let end = format!("-----END {label}-----");
        let count = raw[at..]
            .windows(end.len())
            .position(|v| v == end.as_bytes())
            .ok_or(Error::Refused)?;
        if count == 0
            || !raw[at..at + count]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=\r\n".contains(b))
        {
            return Err(Error::Refused);
        }
        at += count + end.len();
        result.push(&raw[start..at]);
        if result.len() > 3 {
            return Err(Error::Refused);
        }
    }
    if result.is_empty() {
        return Err(Error::Refused);
    }
    Ok(result)
}

pub(super) fn certificates(raw: &[u8]) -> Result<Vec<X509>> {
    frames(raw, &["CERTIFICATE"])?
        .into_iter()
        .map(|frame| {
            let (rest, pem) =
                x509_parser::pem::parse_x509_pem(frame).map_err(|_| Error::Refused)?;
            if !rest.iter().all(|b| whitespace(*b)) || pem.label != "CERTIFICATE" {
                return Err(Error::Refused);
            }
            let (rest, _) = parse_x509_certificate(&pem.contents).map_err(|_| Error::Refused)?;
            if !rest.is_empty() {
                return Err(Error::Refused);
            }
            X509::from_der(&pem.contents).map_err(|_| Error::Refused)
        })
        .collect()
}
fn key_matches(certificate: &X509, raw: &[u8]) -> Result<()> {
    if frames(raw, &["PRIVATE KEY", "EC PRIVATE KEY", "RSA PRIVATE KEY"])?.len() != 1 {
        return Err(Error::Refused);
    }
    let private = PKey::private_key_from_pem(raw).map_err(|_| Error::Refused)?;
    let actual = private.public_key_to_der().map_err(|_| Error::Refused)?;
    let expected = certificate
        .public_key()
        .and_then(|key| key.public_key_to_der())
        .map_err(|_| Error::Refused)?;
    if actual != expected {
        return Err(Error::Refused);
    }
    Ok(())
}

fn complete_fields(oid: &str, object: &DerObject<'_>) -> Result<()> {
    match oid {
        // These two x509-parser field parsers return an inner remainder that
        // der-parser's container intentionally discards. The mature DER tree
        // preserves every child, so require the complete fixed field shapes.
        "1.3.6.1.5.5.7.1.1" | "1.3.6.1.5.5.7.1.11" => {
            for description in object.as_sequence().map_err(|_| Error::Refused)? {
                let fields = description.as_sequence().map_err(|_| Error::Refused)?;
                if fields.len() != 2
                    || fields[0].as_oid().is_err()
                    || fields[1].class() != Class::ContextSpecific
                    || fields[1].tag().0 > 8
                {
                    return Err(Error::Refused);
                }
            }
        }
        "2.5.29.35" => {
            let mut previous = None;
            for field in object.as_sequence().map_err(|_| Error::Refused)? {
                let tag = field.tag().0;
                if field.class() != Class::ContextSpecific
                    || tag > 2
                    || previous.is_some_and(|previous| previous >= tag)
                {
                    return Err(Error::Refused);
                }
                previous = Some(tag);
                if tag == 1 {
                    use x509_parser::asn1_rs::FromDer;
                    let mut bytes = field.as_slice().map_err(|_| Error::Refused)?;
                    while !bytes.is_empty() {
                        let (rest, name) =
                            GeneralName::from_der(bytes).map_err(|_| Error::Refused)?;
                        if rest.len() >= bytes.len() {
                            return Err(Error::Refused);
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

fn general_name(name: &GeneralName<'_>) -> Result<()> {
    match name {
        GeneralName::Invalid(..) | GeneralName::X400Address(_) | GeneralName::EDIPartyName(_) => {
            Err(Error::Refused)
        }
        GeneralName::IPAddress(bytes) if ![4, 16].contains(&bytes.len()) => Err(Error::Refused),
        GeneralName::RegisteredID(oid) => oid_value(oid.as_bytes()),
        GeneralName::OtherName(oid, bytes) => {
            oid_value(oid.as_bytes())?;
            let (rest, explicit) =
                x509_parser::der_parser::der::parse_der(bytes).map_err(|_| Error::Refused)?;
            if !rest.is_empty()
                || explicit.class() != Class::ContextSpecific
                || explicit.tag().0 != 0
                || !explicit.header.is_constructed()
            {
                return Err(Error::Refused);
            }
            let (rest, _) = x509_parser::der_parser::der::parse_der(
                explicit.as_slice().map_err(|_| Error::Refused)?,
            )
            .map_err(|_| Error::Refused)?;
            if rest.is_empty() {
                Ok(())
            } else {
                Err(Error::Refused)
            }
        }
        _ => Ok(()),
    }
}

fn oid_value(bytes: &[u8]) -> Result<()> {
    use asn1_rs::Oid;
    use x509_parser::num_bigint::BigUint;
    // Decode the first combined subidentifier with the mature relative iterator:
    // the absolute iterator assumes it fits one byte. The direct optional
    // asn1-rs bigint feature preserves the full arc range without hidden feature
    // unification or a fixed integer ceiling.
    let relative = Oid::new_relative(Cow::Borrowed(bytes));
    let mut arcs = relative.iter_bigint();
    let combined = arcs.next().ok_or(Error::Refused)?;
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
    let native = Asn1Object::from_str(&numeric).map_err(|_| Error::Refused)?;
    if native.as_slice() == bytes {
        Ok(())
    } else {
        Err(Error::Refused)
    }
}

fn profile(certificate: &X509, authority: Option<bool>) -> Result<(i64, i64)> {
    let der = certificate.to_der().map_err(|_| Error::Refused)?;
    let (rest, parsed) = parse_x509_certificate(&der).map_err(|_| Error::Refused)?;
    if !rest.is_empty() {
        return Err(Error::Refused);
    }
    let mut seen = BTreeSet::new();
    let mut basic = None;
    let mut usage = None;
    let mut extended = None;
    let mut names = None;
    for extension in parsed.extensions() {
        let oid = extension.oid.to_id_string();
        if !seen.insert(oid.clone())
            || UNSUPPORTED.contains(&oid.as_str())
            || (extension.critical && !KNOWN.contains(&oid.as_str()))
        {
            return Err(Error::Refused);
        }
        if !matches!(
            extension.parsed_extension(),
            ParsedExtension::UnsupportedExtension { .. }
        ) {
            // x509-parser's extension dispatcher discards each inner parser's
            // remainder. Independently require one complete mature DER value.
            let (rest, object) = x509_parser::der_parser::der::parse_der(extension.value)
                .map_err(|_| Error::Refused)?;
            if !rest.is_empty() {
                return Err(Error::Refused);
            }
            complete_fields(&oid, &object)?;
        } else if KNOWN.contains(&oid.as_str()) {
            return Err(Error::Refused);
        }
        match extension.parsed_extension() {
            ParsedExtension::ParseError { .. } | ParsedExtension::Unparsed => {
                return Err(Error::Refused);
            }
            ParsedExtension::BasicConstraints(value) => basic = Some(value),
            ParsedExtension::KeyUsage(value) => usage = Some(value),
            ParsedExtension::ExtendedKeyUsage(value) => extended = Some(value),
            ParsedExtension::SubjectAlternativeName(value) => {
                for name in &value.general_names {
                    general_name(name)?;
                }
                names = Some(value);
            }
            ParsedExtension::IssuerAlternativeName(value) => {
                for name in &value.general_names {
                    general_name(name)?;
                }
            }
            ParsedExtension::AuthorityInfoAccess(value) => {
                // The safe native decoder independently consumes the complete
                // AIA schema; presence is already established by this extension.
                certificate.authority_info().ok_or(Error::Refused)?;
                for description in &value.accessdescs {
                    oid_value(description.access_method.as_bytes())?;
                    if description.access_method.to_id_string() == "1.3.6.1.5.5.7.48.1" {
                        return Err(Error::Refused);
                    }
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
    let usage = usage.ok_or(Error::Refused)?;
    if let Some(root) = authority {
        let basic = basic.ok_or(Error::Refused)?;
        if !basic.ca
            || !usage.key_cert_sign()
            || (root && basic.path_len_constraint.is_some_and(|n| n < 1))
        {
            return Err(Error::Refused);
        }
    } else {
        let names = &names.ok_or(Error::Refused)?.general_names;
        if names.len() != 1
            || !matches!(&names[0], GeneralName::DNSName(name) if *name == NAME)
            || basic.is_some_and(|v| v.ca)
            || !usage.digital_signature()
            || usage.key_cert_sign()
            || !extended.is_some_and(|v| v.server_auth)
        {
            return Err(Error::Refused);
        }
        // Native full-length DirectoryString conversion also supports BMP/T61.
        // The deprecated as_utf8 truncates interior NULs in this pinned crate.
        let common = certificate
            .subject_name()
            .entries_by_nid(openssl::nid::Nid::COMMONNAME)
            .collect::<Vec<_>>();
        if !common.is_empty()
            && (common.len() != 1
                || common[0].data().to_string().map_err(|_| Error::Refused)? != NAME)
        {
            return Err(Error::Refused);
        }
    }
    Ok((
        parsed.validity().not_before.timestamp(),
        parsed.validity().not_after.timestamp(),
    ))
}
fn issued(certificate: &X509, issuer: &X509) -> Result<()> {
    if certificate
        .issuer_name()
        .try_cmp(issuer.subject_name())
        .map_err(|_| Error::Refused)?
        != Ordering::Equal
    {
        return Err(Error::Refused);
    }
    let public = issuer.public_key().map_err(|_| Error::Refused)?;
    if !certificate.verify(&public).map_err(|_| Error::Refused)? {
        return Err(Error::Refused);
    }
    Ok(())
}
fn micros(now: SystemTime) -> Result<i128> {
    match now.duration_since(UNIX_EPOCH) {
        Ok(value) => i128::try_from(value.as_micros()).map_err(|_| Error::Input),
        Err(value) => i128::try_from(value.duration().as_micros())
            .map(|v| -v)
            .map_err(|_| Error::Input),
    }
}
fn facts(
    certificate: &X509,
    dates: (i64, i64),
    now: i128,
    end: i128,
    leaf: bool,
) -> Result<CertificateFacts> {
    let before = i128::from(dates.0) * 1_000_000;
    let after = i128::from(dates.1) * 1_000_000;
    if !(before <= now && now < end && end < after) {
        return Err(Error::Refused);
    }
    let lifetime = after.checked_sub(before).ok_or(Error::Input)?;
    let (numerator, denominator) = if leaf { (2, 3) } else { (4, 5) };
    let fraction = lifetime.checked_mul(numerator).ok_or(Error::Input)?;
    let mut offset = fraction / denominator;
    if (fraction % denominator) * 2 > denominator {
        offset += 1;
    }
    let renewal = before.checked_add(offset).ok_or(Error::Input)?;
    if end >= renewal || (leaf && after - end <= 50 * 60 * 1_000_000) {
        return Err(Error::Refused);
    }
    let utc = |seconds| {
        DateTime::<Utc>::from_timestamp(seconds, 0)
            .filter(|v| (1..=9999).contains(&v.year()))
            .ok_or(Error::Input)
    };
    let public = certificate
        .public_key()
        .and_then(|key| key.public_key_to_der())
        .map_err(|_| Error::Refused)?;
    Ok(CertificateFacts {
        certificate_sha256: super::render::digest(
            &certificate.to_der().map_err(|_| Error::Refused)?,
        ),
        public_key_sha256: super::render::digest(&public),
        not_before: utc(dates.0)?,
        not_after: utc(dates.1)?,
        lifetime_micros: lifetime,
        remaining_micros: after - now,
        renewal_margin_micros: renewal - end,
    })
}

/// Inspect bytes against the actual sampled UTC clock and original remaining interval.
pub(super) fn analyze(
    materials: &BTreeMap<String, Vec<u8>>,
    now: SystemTime,
    remaining: Duration,
) -> Result<Materials> {
    if materials.len() != PATHS.len() || PATHS.iter().any(|p| !materials.contains_key(*p)) {
        return Err(Error::Input);
    }
    if materials.values().any(|v| v.is_empty() || v.len() > 65536)
        || materials[PATHS[6]].len() > 16384
        || materials.values().map(Vec::len).sum::<usize>() > 458752
    {
        return Err(Error::Bounds);
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Metadata {
        sans: Vec<String>,
        issuer_data: Value,
    }
    let metadata: Metadata =
        serde_json::from_slice(&materials[PATHS[6]]).map_err(|_| Error::Input)?;
    if metadata.sans != [NAME] || !metadata.issuer_data.is_null() {
        return Err(Error::Refused);
    }
    let now = micros(now)?;
    let remaining = i128::try_from(remaining.as_micros()).map_err(|_| Error::Input)?;
    let end = now
        .checked_add(remaining)
        .filter(|v| *v > now)
        .ok_or(Error::Input)?;
    let roots = certificates(&materials[PATHS[0]])?;
    let intermediates = certificates(&materials[PATHS[2]])?;
    let chain = certificates(&materials[PATHS[4]])?;
    if roots.len() != 1 || intermediates.len() != 1 || ![2, 3].contains(&chain.len()) {
        return Err(Error::Refused);
    }
    let [root]: [_; 1] = roots.try_into().map_err(|_| Error::Refused)?;
    let [intermediate]: [_; 1] = intermediates.try_into().map_err(|_| Error::Refused)?;
    let leaf = &chain[0];
    let der = [leaf.to_der(), intermediate.to_der(), root.to_der()]
        .map(|v| v.map_err(|_| Error::Refused));
    let [leaf_der, intermediate_der, root_der] = der;
    let der = [leaf_der?, intermediate_der?, root_der?];
    if chain
        .iter()
        .map(|certificate| certificate.to_der())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::Refused)?
        != der[..chain.len()]
        || der[0] == der[1]
        || der[0] == der[2]
        || der[1] == der[2]
    {
        return Err(Error::Refused);
    }
    key_matches(&root, &materials[PATHS[1]])?;
    key_matches(&intermediate, &materials[PATHS[3]])?;
    key_matches(leaf, &materials[PATHS[5]])?;
    let root_dates = profile(&root, Some(true))?;
    let intermediate_dates = profile(&intermediate, Some(false))?;
    let leaf_dates = profile(leaf, None)?;
    issued(&root, &root)?;
    issued(&intermediate, &root)?;
    issued(leaf, &intermediate)?;
    let certificates = [
        facts(&root, root_dates, now, end, false)?,
        facts(&intermediate, intermediate_dates, now, end, false)?,
        facts(leaf, leaf_dates, now, end, true)?,
    ];
    let der_hashes = der.each_ref().map(|v| super::render::digest(v));
    let result = Materials {
        certificates,
        chain_count: chain.len(),
        der,
        der_hashes,
    };
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
