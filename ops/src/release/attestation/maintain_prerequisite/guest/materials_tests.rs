//! Actual signed bytes and private keys, with independent rejection neighbors.
use super::*;
use openssl::{
    asn1::{Asn1Integer, Asn1Object, Asn1OctetString, Asn1Time},
    bn::BigNum,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    x509::{
        X509Builder, X509Extension, X509NameBuilder,
        extension::{
            AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
            SubjectAlternativeName, SubjectKeyIdentifier,
        },
    },
};

pub(in super::super) struct Corpus {
    pub materials: BTreeMap<String, Vec<u8>>,
    pub now: SystemTime,
    pub root: X509,
    pub intermediate: X509,
    pub leaf: X509,
    pub leaf_key: PKey<Private>,
}
#[derive(Clone, Copy, Default)]
enum Variant {
    #[default]
    Good,
    LeafBc,
    ExtraEku,
    CaIssuers,
    OtherName,
    RegisteredIdZero,
    RegisteredIdLarge,
    WrongName,
    NullCn,
    WrongIssuer,
    WrongSignature,
    MissingKu,
    MissingEku,
    LeafCa,
    RootPathZero,
    DuplicateKu,
    UnknownCritical,
    UnknownNoncritical,
    Unsupported,
    Ocsp,
    MalformedSki,
    MalformedAia,
    MalformedAki,
    AiaInvalidUtf8,
    AiaShortIp,
    AiaUnframedOtherName,
    AiaUnsupportedName,
    AiaEmptyRegisteredId,
    AiaTruncatedRegisteredId,
    Expired,
    Future,
}
fn extra(oid: &str, critical: bool, der: &[u8]) -> X509Extension {
    X509Extension::new_from_der(
        &Asn1Object::from_str(oid).unwrap(),
        critical,
        &Asn1OctetString::new_from_bytes(der).unwrap(),
    )
    .unwrap()
}
fn aia_name(tag: u8, data: &[u8]) -> X509Extension {
    // Small fixed test fixture encoding, never an application DER decoder.
    assert!(data.len() < 100);
    let mut bytes = vec![
        0x30,
        (14 + data.len()) as u8,
        0x30,
        (12 + data.len()) as u8,
        0x06,
        0x08,
        0x2b,
        0x06,
        0x01,
        0x05,
        0x05,
        0x07,
        0x30,
        0x02,
        tag,
        data.len() as u8,
    ];
    bytes.extend(data);
    extra("1.3.6.1.5.5.7.1.1", false, &bytes)
}
fn certificate(
    subject: &str,
    key: &PKey<Private>,
    issuer: Option<(&X509, &PKey<Private>)>,
    serial: u32,
    before: i64,
    after: i64,
    variant: Variant,
) -> X509 {
    let mut builder = X509Builder::new().unwrap();
    builder.set_version(2).unwrap();
    let number = Asn1Integer::from_bn(&BigNum::from_u32(serial).unwrap()).unwrap();
    builder.set_serial_number(&number).unwrap();
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_nid(
        Nid::COMMONNAME,
        if matches!(variant, Variant::NullCn) && serial == 3 {
            "maintain.localhost\0other"
        } else {
            subject
        },
    )
    .unwrap();
    let name = name.build();
    builder.set_subject_name(&name).unwrap();
    if matches!(variant, Variant::WrongIssuer) && serial == 3 {
        builder.set_issuer_name(&name).unwrap();
    } else {
        builder
            .set_issuer_name(issuer.map_or(&*name, |(cert, _)| cert.subject_name()))
            .unwrap();
    }
    builder.set_pubkey(key).unwrap();
    builder
        .set_not_before(&Asn1Time::from_unix(before).unwrap())
        .unwrap();
    builder
        .set_not_after(&Asn1Time::from_unix(after).unwrap())
        .unwrap();
    if serial < 3 {
        let mut constraints = BasicConstraints::new();
        constraints.critical().ca();
        constraints.pathlen(
            if serial == 1 && !matches!(variant, Variant::RootPathZero) {
                1
            } else {
                0
            },
        );
        builder
            .append_extension(constraints.build().unwrap())
            .unwrap();
        builder
            .append_extension(
                KeyUsage::new()
                    .critical()
                    .key_cert_sign()
                    .crl_sign()
                    .build()
                    .unwrap(),
            )
            .unwrap();
    } else {
        if matches!(variant, Variant::LeafBc | Variant::LeafCa) {
            let mut bc = BasicConstraints::new();
            if matches!(variant, Variant::LeafCa) {
                bc.ca();
            }
            builder.append_extension(bc.build().unwrap()).unwrap();
        }
        if !matches!(variant, Variant::MissingKu) {
            let ku = KeyUsage::new()
                .critical()
                .digital_signature()
                .build()
                .unwrap();
            builder.append_extension(ku).unwrap();
            if matches!(variant, Variant::DuplicateKu) {
                builder
                    .append_extension(KeyUsage::new().digital_signature().build().unwrap())
                    .unwrap();
            }
        }
        if !matches!(variant, Variant::MissingEku) {
            let mut eku = ExtendedKeyUsage::new();
            eku.server_auth();
            if matches!(variant, Variant::ExtraEku) {
                eku.client_auth();
            }
            builder.append_extension(eku.build().unwrap()).unwrap();
        }
        let mut san = SubjectAlternativeName::new();
        san.dns(subject);
        let extension = san
            .build(&builder.x509v3_context(issuer.map(|v| &**v.0), None))
            .unwrap();
        builder.append_extension(extension).unwrap();
        let extension = match variant {
            Variant::UnknownCritical => Some(extra("1.2.3.4", true, &[5, 0])),
            Variant::UnknownNoncritical => Some(extra("1.2.3.4", false, &[5, 0])),
            Variant::Unsupported => Some(extra("2.5.29.30", false, &[0x30, 0])),
            Variant::MalformedSki => Some(extra("2.5.29.14", false, &[5, 0])),
            Variant::MalformedAia => Some(extra(
                "1.3.6.1.5.5.7.1.1",
                false,
                b"\x30\x13\x30\x11\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x02\x86\x03uri\x05\x00",
            )),
            Variant::CaIssuers => Some(extra(
                "1.3.6.1.5.5.7.1.1",
                false,
                b"\x30\x11\x30\x0f\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x02\x86\x03uri",
            )),
            Variant::OtherName => Some(extra("1.3.6.1.5.5.7.1.1",false,b"\x30\x16\x30\x14\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x02\xa0\x08\x06\x02\x2a\x03\xa0\x02\x05\x00")),
            Variant::AiaInvalidUtf8 => Some(extra("1.3.6.1.5.5.7.1.1",false,b"\x30\x0f\x30\x0d\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x02\x86\x01\xff")),
            Variant::AiaShortIp => Some(extra("1.3.6.1.5.5.7.1.1",false,b"\x30\x0f\x30\x0d\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x02\x87\x01\x01")),
            Variant::AiaUnframedOtherName => Some(extra("1.3.6.1.5.5.7.1.1",false,b"\x30\x14\x30\x12\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x02\xa0\x06\x06\x02\x2a\x03\x05\x00")),
            Variant::AiaUnsupportedName => Some(extra("1.3.6.1.5.5.7.1.1",false,b"\x30\x0e\x30\x0c\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x02\xa3\x00")),
            Variant::RegisteredIdZero=>Some(aia_name(0x88,&[0])),
            Variant::RegisteredIdLarge=>Some(aia_name(0x88,Asn1Object::from_str("2.999999999999999999999999999999999999.1").unwrap().as_slice())),
            Variant::AiaEmptyRegisteredId=>Some(aia_name(0x88,&[])),
            Variant::AiaTruncatedRegisteredId=>Some(aia_name(0x88,&[0x2a,0x80])),
            Variant::Ocsp => Some(extra(
                "1.3.6.1.5.5.7.1.1",
                false,
                b"\x30\x1e\x30\x1c\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x01\x86\x10http://ocsp.test",
            )),
            _ => None,
        };
        if let Some(extension) = extension {
            builder.append_extension(extension).unwrap();
        }
    }
    if !matches!(variant, Variant::MalformedSki) || serial != 3 {
        let ski = SubjectKeyIdentifier::new()
            .build(&builder.x509v3_context(issuer.map(|v| &**v.0), None))
            .unwrap();
        builder.append_extension(ski).unwrap();
    }
    if let Some((cert, _)) = issuer {
        let aki = if matches!(variant, Variant::MalformedAki) && serial == 3 {
            extra("2.5.29.35", false, b"\x30\x04\x80\x00\x05\x00")
        } else {
            AuthorityKeyIdentifier::new()
                .keyid(true)
                .build(&builder.x509v3_context(Some(cert), None))
                .unwrap()
        };
        builder.append_extension(aki).unwrap();
    }
    let signing = if matches!(variant, Variant::WrongSignature) && serial == 3 {
        key
    } else {
        issuer.map_or(key, |v| v.1)
    };
    let digest = if [openssl::pkey::Id::ED25519, openssl::pkey::Id::ED448].contains(&signing.id()) {
        MessageDigest::null()
    } else {
        MessageDigest::sha256()
    };
    builder.sign(signing, digest).unwrap();
    builder.build()
}
fn corpus_with(variant: Variant, curve: Nid) -> Corpus {
    let group = EcGroup::from_curve_name(curve).unwrap();
    let key = || PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    corpus_keys(variant, [key(), key(), key()])
}
fn corpus_keys(variant: Variant, keys: [PKey<Private>; 3]) -> Corpus {
    let now = SystemTime::now();
    let seconds = i64::try_from(now.duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
    let [root_key, intermediate_key, leaf_key] = keys;
    let root = certificate(
        "fixture root",
        &root_key,
        None,
        1,
        seconds - 3600,
        seconds + 30 * 86400,
        variant,
    );
    let intermediate = certificate(
        "fixture intermediate",
        &intermediate_key,
        Some((&root, &root_key)),
        2,
        seconds - 1800,
        seconds + 10 * 86400,
        variant,
    );
    let name = if matches!(variant, Variant::WrongName) {
        "other.localhost"
    } else {
        NAME
    };
    let (before, after) = if matches!(variant, Variant::Expired) {
        (seconds - 7200, seconds - 1)
    } else if matches!(variant, Variant::Future) {
        (seconds + 60, seconds + 21600)
    } else {
        (seconds - 300, seconds + 21600)
    };
    let leaf = certificate(
        name,
        &leaf_key,
        Some((&intermediate, &intermediate_key)),
        3,
        before,
        after,
        variant,
    );
    let mut chain = leaf.to_pem().unwrap();
    chain.extend(intermediate.to_pem().unwrap());
    let values = [
        root.to_pem().unwrap(),
        root_key.private_key_to_pem_pkcs8().unwrap(),
        intermediate.to_pem().unwrap(),
        intermediate_key.private_key_to_pem_pkcs8().unwrap(),
        chain,
        leaf_key.private_key_to_pem_pkcs8().unwrap(),
        br#"{"sans":["maintain.localhost"],"issuer_data":null}"#.to_vec(),
    ];
    Corpus {
        materials: PATHS
            .into_iter()
            .zip(values)
            .map(|(p, v)| (p.into(), v))
            .collect(),
        now,
        root,
        intermediate,
        leaf,
        leaf_key,
    }
}
pub(in super::super) fn corpus() -> Corpus {
    corpus_with(Variant::Good, Nid::X9_62_PRIME256V1)
}
pub(in super::super) fn wrong_name_corpus() -> Corpus {
    corpus_with(Variant::WrongName, Nid::X9_62_PRIME256V1)
}

#[test]
fn natural_missing_leaf_bc_extra_eku_unknown_noncritical_and_p521_are_supported() {
    for variant in [
        Variant::Good,
        Variant::LeafBc,
        Variant::ExtraEku,
        Variant::CaIssuers,
        Variant::OtherName,
        Variant::RegisteredIdZero,
        Variant::RegisteredIdLarge,
        Variant::UnknownNoncritical,
    ] {
        let fixture = corpus_with(variant, Nid::X9_62_PRIME256V1);
        let result = analyze(&fixture.materials, fixture.now, Duration::from_secs(600)).unwrap();
        assert_eq!(result.facts()["metadataIssuerDataNull"], true);
        assert_eq!(result.der[0], fixture.leaf.to_der().unwrap());
        assert_eq!(result.der_hashes.len(), 3);
    }
    let fixture = corpus_with(Variant::Good, Nid::SECP521R1);
    assert!(analyze(&fixture.materials, fixture.now, Duration::from_secs(600)).is_ok());
}
#[test]
fn rsa_and_ed448_keys_use_the_same_complete_signature_and_spki_policy() {
    let rsa = PKey::from_rsa(openssl::rsa::Rsa::generate(2048).unwrap()).unwrap();
    let ed448 = PKey::generate_ed448().unwrap();
    for key in [rsa, ed448] {
        let fixture = corpus_keys(Variant::Good, [key.clone(), key.clone(), key]);
        assert!(analyze(&fixture.materials, fixture.now, Duration::from_secs(600)).is_ok());
    }
}
#[test]
fn real_signed_certificate_policy_and_date_neighbors_are_refused() {
    for variant in [
        Variant::WrongName,
        Variant::NullCn,
        Variant::WrongIssuer,
        Variant::WrongSignature,
        Variant::MissingKu,
        Variant::MissingEku,
        Variant::LeafCa,
        Variant::RootPathZero,
        Variant::DuplicateKu,
        Variant::UnknownCritical,
        Variant::Unsupported,
        Variant::Ocsp,
        Variant::MalformedSki,
        Variant::MalformedAia,
        Variant::MalformedAki,
        Variant::AiaInvalidUtf8,
        Variant::AiaShortIp,
        Variant::AiaUnframedOtherName,
        Variant::AiaUnsupportedName,
        Variant::AiaEmptyRegisteredId,
        Variant::AiaTruncatedRegisteredId,
        Variant::Expired,
        Variant::Future,
    ] {
        let fixture = corpus_with(variant, Nid::X9_62_PRIME256V1);
        assert!(analyze(&fixture.materials, fixture.now, Duration::from_secs(600)).is_err());
    }
}
#[test]
fn native_oid_round_trip_keeps_large_arcs_and_refuses_empty_truncated_nonminimal_values() {
    for numeric in [
        "0.0",
        "1.2.840.113549",
        "2.48.1",
        "2.999999999999999999999999999999999999.1",
    ] {
        let native = Asn1Object::from_str(numeric).unwrap();
        oid_value(native.as_slice()).unwrap();
    }
    for bytes in [
        &[][..],
        &[0x80][..],
        &[0x2a, 0x80][..],
        &[0x2a, 0x80, 0x01][..],
        &[0x80, 0x00][..],
    ] {
        assert!(oid_value(bytes).is_err());
    }
}
#[test]
fn complete_der_tree_refuses_inner_fields_the_extension_parser_discards() {
    for (variant, oid) in [
        (Variant::MalformedAia, "1.3.6.1.5.5.7.1.1"),
        (Variant::MalformedAki, "2.5.29.35"),
    ] {
        let fixture = corpus_with(variant, Nid::X9_62_PRIME256V1);
        let der = fixture.leaf.to_der().unwrap();
        let (_, certificate) = parse_x509_certificate(&der).unwrap();
        let extension = certificate
            .extensions()
            .iter()
            .find(|extension| extension.oid.to_id_string() == oid)
            .unwrap();
        assert!(matches!(
            extension.parsed_extension(),
            ParsedExtension::AuthorityInfoAccess(_) | ParsedExtension::AuthorityKeyIdentifier(_)
        ));
        let (rest, object) = x509_parser::der_parser::der::parse_der(extension.value).unwrap();
        assert!(rest.is_empty()); // Outer framing alone cannot establish complete fields.
        assert!(complete_fields(oid, &object).is_err());
        assert!(analyze(&fixture.materials, fixture.now, Duration::from_secs(600)).is_err());
    }
}
#[test]
fn strict_framing_metadata_order_key_pairs_and_der_consumption_are_checked() {
    let fixture = corpus();
    let mut trailing_der = fixture.root.to_der().unwrap();
    trailing_der.push(0);
    let trailing_der = format!(
        "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
        openssl::base64::encode_block(&trailing_der)
    )
    .into_bytes();
    for (path, value) in [
        (
            PATHS[0],
            [fixture.materials[PATHS[0]].clone(), b"secret text".to_vec()].concat(),
        ),
        (PATHS[0], trailing_der),
        (
            PATHS[1],
            [
                fixture.materials[PATHS[1]].clone(),
                fixture.materials[PATHS[1]].clone(),
            ]
            .concat(),
        ),
        (PATHS[1], fixture.materials[PATHS[5]].clone()),
        (
            PATHS[4],
            [
                fixture.leaf.to_pem().unwrap(),
                fixture.root.to_pem().unwrap(),
            ]
            .concat(),
        ),
        (
            PATHS[6],
            br#"{"sans":["maintain.localhost"],"issuer_data":null,"issuer_data":null}"#.to_vec(),
        ),
        (PATHS[6], br#"{"sans":["maintain.localhost"]}"#.to_vec()),
        (
            PATHS[6],
            br#"{"sans":["maintain.localhost"],"issuer_data":NaN}"#.to_vec(),
        ),
    ] {
        let mut changed = fixture.materials.clone();
        changed.insert(path.into(), value);
        assert!(
            analyze(&changed, fixture.now, Duration::from_secs(600)).is_err(),
            "{path}"
        );
    }
    let mut with_root = fixture.materials.clone();
    with_root
        .get_mut(PATHS[4])
        .unwrap()
        .extend(fixture.root.to_pem().unwrap());
    assert!(analyze(&with_root, fixture.now, Duration::from_secs(600)).is_ok());
    assert!(analyze(&fixture.materials, fixture.now, Duration::ZERO).is_err());
}
#[test]
fn exact_renewal_and_fifty_minute_margins_are_strict_without_lifetime_defaults() {
    let fixture = corpus();
    let seconds = i64::try_from(fixture.now.duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
    let now = i128::from(seconds) * 1_000_000;
    let before = seconds - 60;
    let after = before + 7200;
    let end = i128::from(after) * 1_000_000 - 3000 * 1_000_000;
    assert!(facts(&fixture.leaf, (before, after), now, end, true).is_err());
    assert!(facts(&fixture.leaf, (before, after), now, end - 1, true).is_ok());
    let before = seconds - 3600;
    let after = before + 10800;
    let renewal = i128::from(before + 7200) * 1_000_000;
    assert!(facts(&fixture.leaf, (before, after), now, renewal, true).is_err());
    assert!(facts(&fixture.leaf, (before, after), now, renewal - 1, true).is_ok());
}
