//! The reference analyzer's verdicts over this fixture's variants, replayed.
//!
//! `recorded_materials.json` holds what the reference concluded for every variant this fixture
//! builds: the code of a refusal, the class of an error it raised outside its vocabulary, or a
//! passed analysis without the values that depend on the keys or on the analyzer's identity.
//! Two recordings over one set of keys and a third over fresh keys were byte for byte the
//! same, so no verdict depends on the keys.

use super::*;
use sha2::{Digest, Sha256};

const RECORDING: &str = include_str!("recorded_materials.json");
/// The recorder's printed tallies: variants passed, variants refused, refusals per code.
const PASSED: usize = 35;
const REFUSED: usize = 181;
const REFUSED_BY_CODE: [(&str, usize); 20] = [
    ("authority_constraints", 5),
    ("authority_path_length", 2),
    ("certificate_chain", 11),
    ("certificate_extension", 8),
    ("certificate_ocsp", 3),
    ("certificate_pem", 5),
    ("certificate_renewal_window", 18),
    ("certificate_signature", 6),
    ("certificate_structure", 21),
    ("certificate_validity", 11),
    ("fixture_clock", 5),
    ("leaf_constraints", 10),
    ("leaf_subject", 13),
    ("material_set", 4),
    ("material_size", 4),
    ("metadata_duplicate", 10),
    ("metadata_json", 17),
    ("metadata_schema", 17),
    ("private_key_mismatch", 7),
    ("private_key_pem", 4),
];
/// The reference names its libraries' versions where this analyzer names itself. The
/// difference is one of identity, counted once for every passed variant.
const IDENTITY: [(&str, &str); 2] = [
    ("analyzer", "cryptographyVersion"),
    ("opensslVersion", "sslVersion"),
];
/// The variants the reference did not refuse with a code: its certificate library raised its
/// own error for an X.400 address it cannot represent, which the reference's callers report as
/// an unexpected failure. This analyzer refuses the structure. Counted.
const RAISED: [(&str, &str, Refusal); 1] = [(
    "AiaUnsupportedName",
    "UnsupportedGeneralNameType",
    Refusal::CertificateStructure,
)];
/// Values that depend on the keys; they are checked against the built certificates instead.
const KEYED: [&str; 2] = ["certificateSha256", "publicKeySha256"];

/// What the recording keeps of one analysis: every key, and every value except the keyed ones
/// and the identity.
fn retained(facts: &Value) -> Value {
    let mut kept = serde_json::Map::new();
    let mut keys = facts
        .as_object()
        .unwrap()
        .keys()
        .map(|key| {
            IDENTITY
                .iter()
                .find(|(own, _)| own == key)
                .map_or(key.as_str(), |(_, theirs)| theirs)
        })
        .collect::<Vec<_>>();
    keys.sort_unstable();
    kept.insert("keys".into(), json!(keys));
    for (key, value) in facts.as_object().unwrap() {
        if IDENTITY.iter().any(|(own, _)| own == key) {
            continue;
        }
        let Some(certificate) = value.as_object() else {
            kept.insert(key.clone(), value.clone());
            continue;
        };
        let mut fields = serde_json::Map::new();
        let mut names = certificate.keys().collect::<Vec<_>>();
        names.sort_unstable();
        fields.insert("keys".into(), json!(names));
        for (name, field) in certificate {
            if !KEYED.contains(&name.as_str()) {
                fields.insert(name.clone(), field.clone());
            }
        }
        kept.insert(key.clone(), Value::Object(fields));
    }
    Value::Object(kept)
}
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn every_recorded_variant_ends_as_the_reference_ended() {
    let recording: Value = serde_json::from_str(RECORDING).unwrap();
    let verdicts = recording["variants"].as_object().unwrap();
    // A recorded variant this fixture cannot build, or a variant without a recording, fails.
    assert_eq!(
        verdicts.keys().cloned().collect::<BTreeSet<_>>(),
        Variant::ALL
            .iter()
            .map(|variant| format!("{variant:?}"))
            .collect::<BTreeSet<_>>()
    );
    let (mut passed, mut refused, mut identity) = (0, 0, 0);
    let mut by_code = BTreeMap::<String, usize>::new();
    let mut raised = Vec::new();
    for variant in Variant::ALL {
        let name = format!("{variant:?}");
        let fixture = built(*variant);
        let result = inspect(&fixture.materials, fixture.now, fixture.remaining);
        let verdict = verdicts[&name].as_object().unwrap();
        assert_eq!(verdict.len(), 1, "{name}");
        if let Some(error) = verdict.get("raised") {
            let (_, _, expected) = RAISED
                .iter()
                .find(|(variant, class, _)| *variant == name && Some(*class) == error.as_str())
                .unwrap_or_else(|| panic!("{name} raised {error} without a named exception"));
            assert_eq!(result.err(), Some(*expected), "{name}");
            raised.push(name);
            continue;
        }
        match (verdict.get("refused"), result) {
            (Some(code), Err(refusal)) => {
                assert_eq!(Some(refusal.code()), code.as_str(), "{name}");
                refused += 1;
                *by_code.entry(refusal.to_string()).or_default() += 1;
            }
            (None, Ok(materials)) => {
                let facts = materials.facts();
                assert_eq!(retained(&facts), verdict["passed"], "{name}");
                identity += usize::from(
                    IDENTITY
                        .iter()
                        .all(|(own, _)| facts.get(own).is_some_and(Value::is_string)),
                );
                for (role, certificate) in [
                    ("root", &fixture.root),
                    ("intermediate", &fixture.intermediate),
                    ("leaf", &fixture.leaf),
                ] {
                    let public = certificate.public_key().unwrap();
                    assert_eq!(
                        facts[role]["certificateSha256"],
                        sha256(&certificate.to_der().unwrap())
                    );
                    assert_eq!(
                        facts[role]["publicKeySha256"],
                        sha256(&public.public_key_to_der().unwrap())
                    );
                }
                let text = facts.to_string();
                for private in [
                    "PRIVATE KEY",
                    "fixture root",
                    "fixture intermediate",
                    "issuer_data",
                    "https://",
                    "\"passed\"",
                ] {
                    assert!(!text.contains(private), "{name}");
                }
                passed += 1;
            }
            (Some(code), Ok(_)) => panic!("{name} passed where the reference refused {code}"),
            (None, Err(refusal)) => panic!("{name} refused {refusal} where the reference passed"),
        }
    }
    assert_eq!((passed, refused, identity), (PASSED, REFUSED, PASSED));
    assert_eq!(raised, RAISED.map(|(variant, _, _)| variant));
    assert_eq!(
        by_code,
        REFUSED_BY_CODE
            .iter()
            .map(|(code, count)| ((*code).to_owned(), *count))
            .collect()
    );
}
#[test]
fn the_refusal_vocabulary_is_the_references_closed_set() {
    use Refusal as R;
    // The reference's literal codes, in the order its checks first reach them.
    let vocabulary = [
        (R::FixtureClock, "fixture_clock"),
        (R::AnalyzerPrerequisite, "analyzer_prerequisite"),
        (R::MaterialSet, "material_set"),
        (R::MaterialSize, "material_size"),
        (R::MetadataJson, "metadata_json"),
        (R::MetadataDuplicate, "metadata_duplicate"),
        (R::MetadataSchema, "metadata_schema"),
        (R::CertificatePem, "certificate_pem"),
        (R::CertificateChain, "certificate_chain"),
        (R::CertificateStructure, "certificate_structure"),
        (R::CertificateExtension, "certificate_extension"),
        (R::CertificateOcsp, "certificate_ocsp"),
        (R::PrivateKeyPem, "private_key_pem"),
        (R::PrivateKeyMismatch, "private_key_mismatch"),
        (R::AuthorityConstraints, "authority_constraints"),
        (R::AuthorityPathLength, "authority_path_length"),
        (R::LeafConstraints, "leaf_constraints"),
        (R::LeafSubject, "leaf_subject"),
        (R::CertificateSignature, "certificate_signature"),
        (R::CertificateValidity, "certificate_validity"),
        (R::CertificateRenewalWindow, "certificate_renewal_window"),
    ];
    // Exhaustive: a refusal added without its place in the list above does not compile.
    let place = |refusal: Refusal| match refusal {
        R::FixtureClock => 0,
        R::AnalyzerPrerequisite => 1,
        R::MaterialSet => 2,
        R::MaterialSize => 3,
        R::MetadataJson => 4,
        R::MetadataDuplicate => 5,
        R::MetadataSchema => 6,
        R::CertificatePem => 7,
        R::CertificateChain => 8,
        R::CertificateStructure => 9,
        R::CertificateExtension => 10,
        R::CertificateOcsp => 11,
        R::PrivateKeyPem => 12,
        R::PrivateKeyMismatch => 13,
        R::AuthorityConstraints => 14,
        R::AuthorityPathLength => 15,
        R::LeafConstraints => 16,
        R::LeafSubject => 17,
        R::CertificateSignature => 18,
        R::CertificateValidity => 19,
        R::CertificateRenewalWindow => 20,
    };
    for (index, (refusal, code)) in vocabulary.iter().enumerate() {
        assert_eq!(place(*refusal), index);
        assert_eq!(refusal.code(), *code);
        assert_eq!(refusal.to_string(), *code);
    }
    let codes = vocabulary.map(|(_, code)| code);
    assert_eq!(codes.iter().collect::<BTreeSet<_>>().len(), codes.len());
    let recording: Value = serde_json::from_str(RECORDING).unwrap();
    for verdict in recording["variants"].as_object().unwrap().values() {
        if let Some(code) = verdict.get("refused") {
            assert!(codes.contains(&code.as_str().unwrap()), "{code}");
        }
    }
}
#[test]
fn analyze_reports_each_refusal_in_the_guest_category_it_always_reported() {
    use Refusal as R;
    for (refusal, error) in [
        (R::FixtureClock, Error::Input),
        (R::AnalyzerPrerequisite, Error::Input),
        (R::MaterialSet, Error::Input),
        (R::MaterialSize, Error::Bounds),
        (R::MetadataJson, Error::Input),
        (R::CertificatePem, Error::Refused),
        (R::CertificateChain, Error::Refused),
        (R::CertificateStructure, Error::Refused),
        (R::CertificateExtension, Error::Refused),
        (R::CertificateOcsp, Error::Refused),
        (R::PrivateKeyPem, Error::Refused),
        (R::PrivateKeyMismatch, Error::Refused),
        (R::AuthorityConstraints, Error::Refused),
        (R::AuthorityPathLength, Error::Refused),
        (R::LeafConstraints, Error::Refused),
        (R::LeafSubject, Error::Refused),
        (R::CertificateSignature, Error::Refused),
        (R::CertificateValidity, Error::Refused),
        (R::CertificateRenewalWindow, Error::Refused),
    ] {
        for document in [&b"[]"[..], br#"{"sans":[],"issuer_data":1}"#] {
            assert_eq!(refusal.error(document), error, "{refusal}");
        }
    }
    // A document of the record's shape with other values is refused; any other is input.
    for (document, error) in [
        (
            &br#"{"sans":["other"],"issuer_data":null}"#[..],
            Error::Refused,
        ),
        (
            br#"{"sans":[],"issuer_data":{"a":1,"a":2}}"#,
            Error::Refused,
        ),
        (br#"{"sans":["maintain.localhost"]}"#, Error::Input),
        (br#"{"sans":[1],"issuer_data":null}"#, Error::Input),
        (br#"{"sans":[],"sans":[],"issuer_data":null}"#, Error::Input),
        (
            br#"{"sans":[],"issuer_data":null,"extra":null}"#,
            Error::Input,
        ),
        (b"[]", Error::Input),
    ] {
        assert_eq!(R::MetadataSchema.error(document), error);
        assert_eq!(R::MetadataDuplicate.error(document), error);
    }
    for variant in Variant::ALL {
        let fixture = built(*variant);
        let metadata = fixture
            .materials
            .get(PATHS[6])
            .map_or(&[][..], Vec::as_slice);
        assert_eq!(
            analyze(&fixture.materials, fixture.now, fixture.remaining).err(),
            inspect(&fixture.materials, fixture.now, fixture.remaining)
                .err()
                .map(|refusal| refusal.error(metadata)),
            "{variant:?}"
        );
    }
}
#[test]
fn the_clock_and_the_renewal_fractions_round_as_the_reference_rounds() {
    let now = UNIX_EPOCH + Duration::from_secs(BASE.unsigned_abs());
    let start = i128::from(BASE) * 1_000_000;
    // The interval is rounded to the nearest microsecond, ties to even.
    for (nanos, micros) in [
        (1, 0),
        (499, 0),
        (500, 0),
        (501, 1),
        (1500, 2),
        (2500, 2),
        (2501, 3),
    ] {
        assert_eq!(
            clock(now, Duration::from_nanos(nanos)),
            Ok((start, start + micros)),
            "{nanos}"
        );
    }
    assert_eq!(clock(now, Duration::ZERO), Err(Refusal::FixtureClock));
    // A clock before the calendar's first instant is refused even when the end lies within it.
    let first = UNIX_EPOCH - Duration::from_secs(62_135_596_800);
    assert_eq!(
        clock(first, Duration::from_secs(1)),
        Ok((-62_135_596_800_000_000, -62_135_596_799_000_000))
    );
    assert_eq!(
        clock(first - Duration::from_micros(1), Duration::from_secs(1)),
        Err(Refusal::FixtureClock)
    );
    // A clock before the epoch is rounded down, as a later one is.
    assert_eq!(
        clock(UNIX_EPOCH - Duration::from_nanos(1), Duration::from_secs(1)),
        Ok((-1, 999_999))
    );
    let last = UNIX_EPOCH + Duration::from_micros(253_402_300_799_999_999);
    assert_eq!(
        clock(last, Duration::from_nanos(500)),
        Ok((253_402_300_799_999_999, 253_402_300_799_999_999))
    );
    assert_eq!(
        clock(last, Duration::from_nanos(501)),
        Err(Refusal::FixtureClock)
    );
    // A 400-year lifetime renews one microsecond past its exact four fifths, and a 150-year
    // lifetime whose exact two thirds round up renews one microsecond before them.
    assert_eq!(
        scaled(12_623_040_000_000_000, AUTHORITY_RENEWAL),
        10_098_432_000_000_001
    );
    assert_eq!(
        scaled(4_733_640_001_000_000, LEAF_RENEWAL),
        3_155_760_000_666_666
    );
    for (lifetime, half) in [(1, 0), (3, 2), (5, 2), (7, 4)] {
        assert_eq!(scaled(lifetime, (1, 2)), half, "{lifetime}");
    }
}
