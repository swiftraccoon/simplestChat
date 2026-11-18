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

#[path = "materials_reference_tests.rs"]
mod reference;

pub(in super::super) struct Corpus {
    pub materials: BTreeMap<String, Vec<u8>>,
    pub now: SystemTime,
    /// The interval the variant is inspected for.
    pub remaining: Duration,
    pub root: X509,
    pub intermediate: X509,
    pub leaf: X509,
    pub leaf_key: PKey<Private>,
}

/// Declares the fixture variants together with the list of every one of them.
macro_rules! variants {
    ($($(#[$meta:meta])* $name:ident,)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Variant {
            $($(#[$meta])* $name,)*
        }
        impl Variant {
            /// Every variant, in declaration order.
            const ALL: &[Self] = &[$(Self::$name,)*];
        }
    };
}
variants! {
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
    /// Three P-521 keys.
    P521,
    /// One RSA key for all three certificates.
    Rsa,
    /// One Ed448 key for all three certificates.
    Ed448,
    /// The keys in their traditional `EC PRIVATE KEY` encoding instead of PKCS #8.
    TraditionalKeys,
    /// The leaf chain also holds the root.
    HeldRoot,
    /// A twelve-hour leaf, ten seconds old.
    TwelveHourLeaf,
    LeafNoSan,
    /// Key usage present with no usage at all.
    LeafNoDigitalSignature,
    LeafSignsCertificates,
    LeafClientAuthOnly,
    /// Critical non-CA basic constraints, and certificate signing.
    LeafBcSignsCertificates,
    /// Critical non-CA basic constraints, and no extended key usage.
    LeafBcNoEku,
    RootNoBc,
    /// The root's basic constraints say it is no CA.
    RootNotCa,
    IntermediateNoBc,
    /// The root's key usage is digital signature only.
    RootNoCertSign,
    ForeignRootSigner,
    ForeignIntermediateSigner,
    ForeignLeafSigner,
    ForeignRootKey,
    ForeignIntermediateKey,
    ForeignLeafKey,
    RootKeyIsLeafKey,
    DoubledRootKey,
    TrailingRootText,
    /// The root's DER followed by one zero byte, in one PEM block.
    TrailingRootDer,
    DoubledRoot,
    ChainExtraLeaf,
    ChainReversed,
    ChainForeignIntermediate,
    ChainLeafOnly,
    ChainSkipsIntermediate,
    /// Leaf, intermediate, root and root again.
    FourCertificateChain,
    /// The leaf is signed by the root, which also stands in for the intermediate.
    RootAsIntermediate,
    SanExtraDns,
    SanWildcard,
    SanIp,
    SanEmail,
    SanUri,
    /// The fixed name in the alternative names, another common name.
    WrongCn,
    /// An empty leaf subject.
    NoCn,
    /// The fixed name as the leaf's common name twice.
    TwoCommonNames,
    /// Name constraints permitting the fixed name.
    NameConstraints,
    /// Name constraints permitting a wildcard name.
    NameConstraintsWildcard,
    /// Name constraints with an empty list of permitted names.
    NameConstraintsEmptyPermitted,
    /// Name constraints excluding the fixed name.
    NameConstraintsExcluded,
    /// Name constraints excluding a wildcard name.
    NameConstraintsExcludedWildcard,
    /// Name constraints with an empty list of excluded names.
    NameConstraintsEmptyExcluded,
    /// Policy constraints with neither constraint.
    PolicyConstraintsEmpty,
    /// Policy constraints requiring an explicit policy at once.
    PolicyConstraints,
    InhibitAnyPolicy,
    /// Certificate policies naming any policy.
    CertificatePolicies,
    PolicyMappings,
    /// Key usage with digital signature and encipher-only, but no key agreement.
    LeafEncipherOnly,
    /// Key usage with digital signature and decipher-only, but no key agreement.
    LeafDecipherOnly,
    /// Key usage with digital signature, key agreement and encipher-only.
    LeafKeyAgreementEncipherOnly,
    /// Critical non-CA basic constraints with a path length.
    LeafBcPathLength,
    /// An authority key identifier naming an issuer without a serial number.
    AkiIssuerWithoutSerial,
    /// An authority key identifier with a serial number but no issuer.
    AkiSerialWithoutIssuer,
    /// The fixed name and a DNS name that is not ASCII.
    SanNonAscii,
    /// The leaf names its issuer with the intermediate's common name in another case.
    IssuerNameCase,
    OcspLeafForeignLeafKey,
    RootNoBcOcspLeaf,
    MetadataNoIssuer,
    MetadataIssuerObject,
    MetadataTwoSans,
    MetadataExtraKey,
    MetadataList,
    MetadataDuplicateSans,
    MetadataNan,
    MetadataInvalidUtf8,
    MetadataUnclosedObjects,
    MetadataDuplicateIssuer,
    MetadataIssuerNan,
    MetadataNestedDuplicate,
    MetadataEscapedDuplicate,
    MetadataSurrogatePairDuplicate,
    MetadataDuplicateTrailing,
    MetadataDuplicateBeforeSyntax,
    MetadataTrailingData,
    MetadataByteOrderMark,
    MetadataLoneSurrogate,
    MetadataDeepValue,
    MetadataHugeNumber,
    /// An integer of 4300 digits.
    MetadataLongestInteger,
    /// An integer of 4301 digits.
    MetadataLongInteger,
    /// A float literal with 4301 integer digits.
    MetadataLongFloat,
    /// A unit separator, the last control character, inside a string.
    MetadataControlCharacter,
    MetadataSpaceInName,
    /// Every escape, literal and number form in one value.
    MetadataEscapes,
    MetadataBadEscape,
    MetadataLeadingZero,
    /// The record laid out with every whitespace character JSON allows.
    MetadataPretty,
    MetadataNameAsString,
    MetadataNestedName,
    MetadataEscapedName,
    /// The record padded with spaces to exactly its bound.
    MetadataAtBound,
    OversizedMetadata,
    /// The root padded with line ends to exactly the material bound.
    RootCertificateAtBound,
    OversizedRootCertificate,
    EmptyRootCertificate,
    /// A root file of whitespace only.
    BlankRootCertificate,
    /// A root valid from 0000-01-01, a year the reference's calendar lacks.
    RootYearZero,
    MissingRootCertificate,
    /// The root under another name: seven materials, one of them unknown.
    RenamedMaterial,
    ExtraMaterial,
    /// The clock before every certificate.
    EarlyClock,
    /// The clock a day on, past the leaf.
    LateClock,
    /// A ten-minute leaf that ends before the fixture does.
    ShortLeaf,
    /// The clock eight hours into a twelve-hour leaf.
    LeafRenewalReached,
    HourLeaf,
    /// A 25-minute intermediate and a 1336-second fixture.
    ShortIntermediate,
    LeafRenewalEarly,
    LeafRenewalAt,
    LeafRenewalLate,
    /// 400 ns short of the leaf's renewal: rounded up onto it.
    LeafRenewalRoundedUp,
    /// 600 ns short of the leaf's renewal: rounded down before it.
    LeafRenewalRoundedDown,
    IntermediateRenewalEarly,
    IntermediateRenewalAt,
    IntermediateRenewalLate,
    RootRenewalEarly,
    RootRenewalAt,
    RootRenewalLate,
    /// Fifty minutes and one microsecond of leaf after the fixture.
    LeafMarginLong,
    LeafMarginAt,
    LeafMarginShort,
    NotBeforeEarly,
    NotBeforeAt,
    NotBeforeLate,
    RootExpiryEarly,
    RootExpiryAt,
    RootExpiryLate,
    /// A 400-year root ending the fixture at the exact four fifths of its life, one
    /// microsecond before the reference's binary64 `0.8` places renewal.
    LongRootRenewalEarly,
    LongRootRenewalAt,
    LongRootRenewalLate,
    /// A 150-year leaf ending the fixture one microsecond before the reference's binary64
    /// `2 / 3` places renewal, which the exact two thirds round one microsecond later.
    LongLeafRenewalEarly,
    LongLeafRenewalAt,
    LongLeafRenewalLate,
    /// The fixture ends one microsecond before the last instant of the calendar.
    CalendarEndEarly,
    CalendarEndAt,
    CalendarEndLate,
    /// A 400 ns interval, rounded to none.
    SubMicrosecondRemaining,
    ZeroRemaining,
    OverflowRemaining,
    ZeroRemainingOversizedRoot,
    // The JSON number grammar.
    MetadataFractionWithoutDigits,
    MetadataExponentWithoutDigits,
    MetadataExponentSignWithoutDigits,
    MetadataMinusAlone,
    MetadataFloatExponentWithoutDigits,
    // Surrogate pairs at the edges of both ranges, observed through repeated keys.
    MetadataPairLowestDuplicate,
    MetadataPairHighestDuplicate,
    MetadataPairLowFirstDuplicate,
    MetadataPairLowLastDuplicate,
    /// A high surrogate followed by another: not joined.
    MetadataPairBeforeLow,
    /// A high surrogate followed by U+E000: not joined.
    MetadataPairAfterLow,
    // PEM framing and base64.
    /// The root framed by vertical tabs and form feeds.
    RootVerticalTabs,
    KeyVerticalTabs,
    /// A space and a tab on the root's first line, before its block.
    RootLeadingSpace,
    KeyLeadingSpace,
    RootSpaceInBody,
    KeySpaceInBody,
    /// A space before a line end of the root's base64 body.
    RootTrailingSpace,
    KeyTrailingSpace,
    /// The root with carriage returns before its line ends.
    RootCrlf,
    KeyCrlf,
    // Two failed checks at once: the recording says which the reference reports.
    /// Unsupported name constraints and an OCSP responder on the leaf.
    UnsupportedAndOcsp,
    /// Unreadable metadata and an unreadable root.
    MetadataAndRootUnreadable,
    /// Two names in the metadata and a chain of the leaf alone.
    SchemaAndChainCount,
    /// A foreign root key and an intermediate with an OCSP responder.
    ForeignRootKeyIntermediateOcsp,
    /// A foreign root key and an intermediate with name constraints.
    ForeignRootKeyIntermediateUnsupported,
    /// A foreign root key and an intermediate key that is no PEM.
    ForeignRootKeyBadIntermediateKey,
    /// A root key that is no PEM and a foreign intermediate key.
    BadRootKeyForeignIntermediateKey,
    /// An intermediate without basic constraints and a leaf with a second name.
    IntermediateNoBcLeafExtraSan,
    /// A leaf with a second name and a root signed by a foreign key.
    SanExtraDnsForeignRootSigner,
    /// A root path length of 0 and an intermediate that is no CA.
    RootPathZeroIntermediateNotCa,
    /// A CA leaf with a second name.
    SanExtraDnsLeafCa,
    /// A leaf without key usage, with a second name.
    MissingKuSanExtraDns,
    /// Another common name and client authentication only.
    WrongCnClientAuthOnly,
    /// A root in its renewal window and an expired leaf.
    RootRenewalExpiredLeaf,
    /// A missing root and an empty root key.
    MissingAndEmptyMaterial,
    /// An oversized root and unreadable metadata.
    OversizedRootInvalidMetadata,
    /// An empty interval and a missing root.
    ZeroRemainingMissingRoot,
    // Two of the three certificates the same, with the chain order kept.
    /// The intermediate as the leaf: a chain of the intermediate twice.
    ChainIntermediateTwice,
    /// The root as the leaf, before the intermediate.
    ChainRootAsLeaf,
    /// The root as the leaf, before the intermediate and the root.
    ChainRootAsLeafHeld,
}

/// Seconds since the epoch of the fixed clock the variants are built at, 2050-01-01T00:00:00Z.
const BASE: i64 = 2_524_608_000;
const DAY: i64 = 86_400;
/// The last second of the calendar, 9999-12-31T23:59:59Z.
const CALENDAR_END: i64 = 253_402_300_799;
/// Name constraints permitting the fixed DNS name, permitting a wildcard, excluding the name.
const PERMITTED: &[u8] = b"\x30\x18\xa0\x16\x30\x14\x82\x12maintain.localhost";
const WILDCARD: &[u8] = b"\x30\x11\xa0\x0f\x30\x0d\x82\x0b*.localhost";
const EXCLUDED: &[u8] = b"\x30\x18\xa1\x16\x30\x14\x82\x12maintain.localhost";
const EXCLUDED_WILDCARD: &[u8] = b"\x30\x11\xa1\x0f\x30\x0d\x82\x0b*.localhost";
/// Any policy, and a mapping of 1.2.3.4 to 1.2.3.5.
const ANY_POLICY: &[u8] = b"\x30\x08\x30\x06\x06\x04\x55\x1d\x20\x00";
/// The fixed name, and the DNS name `é.localhost` in UTF-8.
const NON_ASCII: &[u8] = b"\x30\x22\x82\x12maintain.localhost\x82\x0c\xc3\xa9.localhost";
const MAPPING: &[u8] = b"\x30\x0c\x30\x0a\x06\x03\x2a\x03\x04\x06\x03\x2a\x03\x05";
const OCSP: &[u8] =
    b"\x30\x1e\x30\x1c\x06\x08\x2b\x06\x01\x05\x05\x07\x30\x01\x86\x10http://ocsp.test";

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
fn p256() -> PKey<Private> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
    PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
}
fn named(common: &str) -> openssl::x509::X509Name {
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_nid(Nid::COMMONNAME, common).unwrap();
    name.build()
}
/// The leaf's basic constraints, key usage and extended key usage.
fn leaf_usage(builder: &mut X509Builder, variant: Variant) {
    use Variant as V;
    match variant {
        V::LeafBc | V::LeafCa | V::SanExtraDnsLeafCa => {
            let mut bc = BasicConstraints::new();
            if matches!(variant, V::LeafCa | V::SanExtraDnsLeafCa) {
                bc.ca();
            }
            builder.append_extension(bc.build().unwrap()).unwrap();
        }
        V::LeafBcSignsCertificates | V::LeafBcNoEku => {
            let bc = BasicConstraints::new().critical().build().unwrap();
            builder.append_extension(bc).unwrap();
        }
        V::LeafBcPathLength => {
            builder
                .append_extension(extra("2.5.29.19", true, b"\x30\x03\x02\x01\x00"))
                .unwrap();
        }
        _ => {}
    }
    match variant {
        V::MissingKu | V::MissingKuSanExtraDns => {}
        V::LeafEncipherOnly => {
            builder
                .append_extension(extra("2.5.29.15", true, b"\x03\x02\x00\x81"))
                .unwrap();
        }
        V::LeafDecipherOnly => {
            builder
                .append_extension(extra("2.5.29.15", true, b"\x03\x03\x07\x80\x80"))
                .unwrap();
        }
        V::LeafKeyAgreementEncipherOnly => {
            builder
                .append_extension(extra("2.5.29.15", true, b"\x03\x02\x00\x89"))
                .unwrap();
        }
        V::LeafNoDigitalSignature => {
            builder
                .append_extension(extra("2.5.29.15", true, b"\x03\x01\x00"))
                .unwrap();
        }
        V::LeafSignsCertificates | V::LeafBcSignsCertificates => {
            let ku = KeyUsage::new()
                .critical()
                .digital_signature()
                .key_cert_sign()
                .crl_sign()
                .build()
                .unwrap();
            builder.append_extension(ku).unwrap();
        }
        _ => {
            let ku = KeyUsage::new()
                .critical()
                .digital_signature()
                .build()
                .unwrap();
            builder.append_extension(ku).unwrap();
            if matches!(variant, V::DuplicateKu) {
                builder
                    .append_extension(KeyUsage::new().digital_signature().build().unwrap())
                    .unwrap();
            }
        }
    }
    if !matches!(variant, V::MissingEku | V::LeafBcNoEku) {
        let mut eku = ExtendedKeyUsage::new();
        if !matches!(variant, V::LeafClientAuthOnly | V::WrongCnClientAuthOnly) {
            eku.server_auth();
        }
        if matches!(
            variant,
            V::ExtraEku | V::LeafClientAuthOnly | V::WrongCnClientAuthOnly
        ) {
            eku.client_auth();
        }
        builder.append_extension(eku.build().unwrap()).unwrap();
    }
}
fn certificate(
    subject: &str,
    key: &PKey<Private>,
    issuer: Option<(&X509, &PKey<Private>)>,
    serial: u32,
    (before, after): (i64, i64),
    variant: Variant,
) -> X509 {
    use Variant as V;
    let (root, intermediate, leaf) = (serial == 1, serial == 2, serial == 3);
    let mut builder = X509Builder::new().unwrap();
    builder.set_version(2).unwrap();
    let number = Asn1Integer::from_bn(&BigNum::from_u32(serial).unwrap()).unwrap();
    builder.set_serial_number(&number).unwrap();
    let name = match variant {
        V::NullCn if leaf => named("maintain.localhost\0other"),
        V::WrongCn | V::WrongCnClientAuthOnly if leaf => named("other.localhost"),
        V::NoCn if leaf => X509NameBuilder::new().unwrap().build(),
        V::TwoCommonNames if leaf => {
            let mut name = X509NameBuilder::new().unwrap();
            name.append_entry_by_nid(Nid::COMMONNAME, NAME).unwrap();
            name.append_entry_by_nid(Nid::COMMONNAME, NAME).unwrap();
            name.build()
        }
        _ => named(subject),
    };
    builder.set_subject_name(&name).unwrap();
    if matches!(variant, V::WrongIssuer) && leaf {
        builder.set_issuer_name(&name).unwrap();
    } else if matches!(variant, V::IssuerNameCase) && leaf {
        builder
            .set_issuer_name(&named("Fixture Intermediate"))
            .unwrap();
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
    if !leaf {
        let bare = (root && matches!(variant, V::RootNoBc | V::RootNoBcOcspLeaf))
            || (intermediate
                && matches!(
                    variant,
                    V::IntermediateNoBc | V::IntermediateNoBcLeafExtraSan
                ));
        if (root && matches!(variant, V::RootNotCa))
            || (intermediate && matches!(variant, V::RootPathZeroIntermediateNotCa))
        {
            let constraints = BasicConstraints::new().critical().build().unwrap();
            builder.append_extension(constraints).unwrap();
        } else if !bare {
            let mut constraints = BasicConstraints::new();
            constraints.critical().ca();
            let zero = matches!(variant, V::RootPathZero | V::RootPathZeroIntermediateNotCa);
            constraints.pathlen(if root && !zero { 1 } else { 0 });
            builder
                .append_extension(constraints.build().unwrap())
                .unwrap();
        }
        let mut usage = KeyUsage::new();
        usage.critical();
        if root && matches!(variant, V::RootNoCertSign) {
            usage.digital_signature();
        } else {
            usage.key_cert_sign().crl_sign();
        }
        builder.append_extension(usage.build().unwrap()).unwrap();
        match variant {
            V::ForeignRootKeyIntermediateOcsp if intermediate => {
                builder
                    .append_extension(extra("1.3.6.1.5.5.7.1.1", false, OCSP))
                    .unwrap();
            }
            V::ForeignRootKeyIntermediateUnsupported if intermediate => {
                builder
                    .append_extension(extra("2.5.29.30", false, PERMITTED))
                    .unwrap();
            }
            _ => {}
        }
    } else {
        leaf_usage(&mut builder, variant);
        if matches!(variant, V::SanNonAscii) {
            builder
                .append_extension(extra("2.5.29.17", false, NON_ASCII))
                .unwrap();
        } else if !matches!(variant, V::LeafNoSan) {
            let mut san = SubjectAlternativeName::new();
            san.dns(subject);
            match variant {
                V::SanExtraDns
                | V::IntermediateNoBcLeafExtraSan
                | V::SanExtraDnsForeignRootSigner
                | V::SanExtraDnsLeafCa
                | V::MissingKuSanExtraDns => san.dns("other.localhost"),
                V::SanWildcard => san.dns("*.localhost"),
                V::SanIp => san.ip("127.0.0.1"),
                V::SanEmail => san.email("inert@example.invalid"),
                V::SanUri => san.uri("https://inert.invalid"),
                _ => &mut san,
            };
            let extension = san
                .build(&builder.x509v3_context(issuer.map(|v| &**v.0), None))
                .unwrap();
            builder.append_extension(extension).unwrap();
        }
        let extension = match variant {
            Variant::UnknownCritical => Some(extra("1.2.3.4", true, &[5, 0])),
            Variant::UnknownNoncritical => Some(extra("1.2.3.4", false, &[5, 0])),
            Variant::Unsupported => Some(extra("2.5.29.30", false, &[0x30, 0])),
            Variant::NameConstraints => Some(extra("2.5.29.30", false, PERMITTED)),
            Variant::NameConstraintsWildcard => Some(extra("2.5.29.30", false, WILDCARD)),
            Variant::NameConstraintsEmptyPermitted => {
                Some(extra("2.5.29.30", false, &[0x30, 2, 0xa0, 0]))
            }
            Variant::NameConstraintsExcluded => Some(extra("2.5.29.30", false, EXCLUDED)),
            Variant::NameConstraintsExcludedWildcard => {
                Some(extra("2.5.29.30", false, EXCLUDED_WILDCARD))
            }
            Variant::NameConstraintsEmptyExcluded => {
                Some(extra("2.5.29.30", false, &[0x30, 2, 0xa1, 0]))
            }
            Variant::PolicyConstraintsEmpty => Some(extra("2.5.29.36", false, &[0x30, 0])),
            Variant::PolicyConstraints => {
                Some(extra("2.5.29.36", false, &[0x30, 3, 0x80, 1, 0]))
            }
            Variant::InhibitAnyPolicy => Some(extra("2.5.29.54", false, &[2, 1, 0])),
            Variant::CertificatePolicies => Some(extra("2.5.29.32", false, ANY_POLICY)),
            Variant::PolicyMappings => Some(extra("2.5.29.33", false, MAPPING)),
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
            Variant::Ocsp
            | Variant::OcspLeafForeignLeafKey
            | Variant::RootNoBcOcspLeaf
            | Variant::UnsupportedAndOcsp => Some(extra("1.3.6.1.5.5.7.1.1", false, OCSP)),
            _ => None,
        };
        if let Some(extension) = extension {
            builder.append_extension(extension).unwrap();
        }
        if matches!(variant, V::UnsupportedAndOcsp) {
            builder
                .append_extension(extra("2.5.29.30", false, PERMITTED))
                .unwrap();
        }
    }
    if !matches!(variant, V::MalformedSki) || !leaf {
        let ski = SubjectKeyIdentifier::new()
            .build(&builder.x509v3_context(issuer.map(|v| &**v.0), None))
            .unwrap();
        builder.append_extension(ski).unwrap();
    }
    if let Some((cert, _)) = issuer {
        let aki = if matches!(variant, V::MalformedAki) && leaf {
            extra("2.5.29.35", false, b"\x30\x04\x80\x00\x05\x00")
        } else if matches!(variant, V::AkiIssuerWithoutSerial) && leaf {
            extra("2.5.29.35", false, b"\x30\x08\x80\x01\x01\xa1\x03\x82\x01x")
        } else if matches!(variant, V::AkiSerialWithoutIssuer) && leaf {
            extra("2.5.29.35", false, b"\x30\x06\x80\x01\x01\x82\x01\x01")
        } else {
            AuthorityKeyIdentifier::new()
                .keyid(true)
                .build(&builder.x509v3_context(Some(cert), None))
                .unwrap()
        };
        builder.append_extension(aki).unwrap();
    }
    let foreign = match variant {
        V::ForeignRootSigner | V::SanExtraDnsForeignRootSigner if root => Some(p256()),
        V::ForeignIntermediateSigner if intermediate => Some(p256()),
        V::ForeignLeafSigner if leaf => Some(p256()),
        _ => None,
    };
    let signing = if matches!(variant, V::WrongSignature) && leaf {
        key
    } else {
        foreign
            .as_ref()
            .unwrap_or_else(|| issuer.map_or(key, |v| v.1))
    };
    let digest = if [openssl::pkey::Id::ED25519, openssl::pkey::Id::ED448].contains(&signing.id()) {
        MessageDigest::null()
    } else {
        MessageDigest::sha256()
    };
    builder.sign(signing, digest).unwrap();
    builder.build()
}

/// When one variant's certificates are valid, its clock in microseconds and its interval.
struct Plan {
    root: (i64, i64),
    intermediate: (i64, i64),
    leaf: (i64, i64),
    now: i128,
    remaining: Duration,
}
fn plan(variant: Variant, base: i64) -> Plan {
    use Variant as V;
    let micros = |seconds: i64| i128::from(seconds) * 1_000_000;
    // Every interval below is whole microseconds unless it names nanoseconds.
    let interval = |micros: u64| Duration::from_micros(micros);
    let around = |base: i64| Plan {
        root: (base - 3600, base + 30 * DAY),
        intermediate: (base - 1800, base + 10 * DAY),
        leaf: (base - 300, base + 21_600),
        now: micros(base),
        remaining: Duration::from_secs(600),
    };
    let mut plan = around(base);
    match variant {
        V::Expired => plan.leaf = (base - 7200, base - 1),
        // The root renews 80 s after the clock; the leaf ended a second before it.
        V::RootRenewalExpiredLeaf => {
            plan.root = (base - 3600, base + 1000);
            plan.leaf = (base - 7200, base - 1);
        }
        V::Future => plan.leaf = (base + 60, base + 21_600),
        V::TwelveHourLeaf => plan.leaf = (base - 10, base - 10 + 43_200),
        V::EarlyClock => plan.now = micros(base - 3660),
        V::LateClock => plan.now = micros(base + DAY),
        V::ShortLeaf => plan.leaf = (base - 10, base + 590),
        V::LeafRenewalReached => {
            plan.leaf = (base - 10, base - 10 + 43_200);
            plan.now = micros(base + 8 * 3600);
        }
        V::HourLeaf => plan.leaf = (base - 10, base + 3590),
        V::ShortIntermediate => {
            plan.intermediate = (base - 10, base + 1490);
            plan.remaining = Duration::from_secs(1336);
        }
        // The default leaf renews two thirds into its 21 900 s: 14 300 s after the clock.
        V::LeafRenewalEarly => plan.remaining = interval(14_300_000_000 - 1),
        V::LeafRenewalAt => plan.remaining = interval(14_300_000_000),
        V::LeafRenewalLate => plan.remaining = interval(14_300_000_000 + 1),
        V::LeafRenewalRoundedUp => plan.remaining = Duration::from_nanos(14_300_000_000_000 - 400),
        V::LeafRenewalRoundedDown => {
            plan.remaining = Duration::from_nanos(14_300_000_000_000 - 600);
        }
        // A 5400 s intermediate from 1800 s ago renews 2520 s after the clock.
        V::IntermediateRenewalEarly | V::IntermediateRenewalAt | V::IntermediateRenewalLate => {
            plan.intermediate = (base - 1800, base + 3600);
            plan.remaining = interval(match variant {
                V::IntermediateRenewalEarly => 2_520_000_000 - 1,
                V::IntermediateRenewalAt => 2_520_000_000,
                _ => 2_520_000_000 + 1,
            });
        }
        // A 10 800 s root from 3600 s ago renews 5040 s after the clock.
        V::RootRenewalEarly | V::RootRenewalAt | V::RootRenewalLate => {
            plan.root = (base - 3600, base + 7200);
            plan.remaining = interval(match variant {
                V::RootRenewalEarly => 5_040_000_000 - 1,
                V::RootRenewalAt => 5_040_000_000,
                _ => 5_040_000_000 + 1,
            });
        }
        // A 7200 s leaf from 60 s ago leaves fifty minutes after 4140 s.
        V::LeafMarginLong | V::LeafMarginAt | V::LeafMarginShort => {
            plan.leaf = (base - 60, base + 7140);
            plan.remaining = interval(match variant {
                V::LeafMarginLong => 4_140_000_000 - 1,
                V::LeafMarginAt => 4_140_000_000,
                _ => 4_140_000_000 + 1,
            });
        }
        V::NotBeforeEarly => plan.now = micros(base - 300) - 1,
        V::NotBeforeAt => plan.now = micros(base - 300),
        V::NotBeforeLate => plan.now = micros(base - 300) + 1,
        V::RootYearZero => plan.root = (-62_167_219_200, base + 30 * DAY),
        V::RootExpiryEarly | V::RootExpiryAt | V::RootExpiryLate => {
            plan.root = (base - 3600, base + 1000);
            plan.remaining = interval(match variant {
                V::RootExpiryEarly => 1_000_000_000 - 1,
                V::RootExpiryAt => 1_000_000_000,
                _ => 1_000_000_000 + 1,
            });
        }
        // 12 623 040 000 s times binary64 0.8 is 10 098 432 000 s and 0.56 µs, rounded to
        // one microsecond past the exact four fifths.
        V::LongRootRenewalEarly | V::LongRootRenewalAt | V::LongRootRenewalLate => {
            let now = base + 10_098_432_000 - 600;
            plan = around(now);
            plan.root = (base, base + 12_623_040_000);
            plan.remaining = interval(match variant {
                V::LongRootRenewalEarly => 600_000_000,
                V::LongRootRenewalAt => 600_000_001,
                _ => 600_000_002,
            });
        }
        // 4 733 640 001 s times binary64 2/3 is 3 155 760 000.666666 49 s, rounded down;
        // the exact two thirds end in .666666 67 and round up.
        V::LongLeafRenewalEarly | V::LongLeafRenewalAt | V::LongLeafRenewalLate => {
            let now = base + 3_155_759_400;
            plan = around(now);
            plan.leaf = (base, base + 4_733_640_001);
            plan.remaining = interval(match variant {
                V::LongLeafRenewalEarly => 600_666_666 - 1,
                V::LongLeafRenewalAt => 600_666_666,
                _ => 600_666_666 + 1,
            });
        }
        // Every certificate ends with the calendar's last second, 999 999 µs before its end.
        V::CalendarEndEarly | V::CalendarEndAt | V::CalendarEndLate => {
            let now = CALENDAR_END - 600;
            plan = around(now);
            plan.root = (now - 3600, CALENDAR_END);
            plan.intermediate = (now - 1800, CALENDAR_END);
            plan.leaf = (now - 300, CALENDAR_END);
            plan.now = micros(now) + 999_999;
            plan.remaining = interval(match variant {
                V::CalendarEndEarly => 600_000_000 - 1,
                V::CalendarEndAt => 600_000_000,
                _ => 600_000_000 + 1,
            });
        }
        V::SubMicrosecondRemaining => plan.remaining = Duration::from_nanos(400),
        V::ZeroRemaining | V::ZeroRemainingOversizedRoot | V::ZeroRemainingMissingRoot => {
            plan.remaining = Duration::ZERO;
        }
        V::OverflowRemaining => plan.remaining = Duration::MAX,
        _ => {}
    }
    plan
}
/// The JSON escape character, never typed before a `u` in this file.
const BACKSLASH: char = '\\';
/// JSON unicode escapes of UTF-16 code units, written from their values.
fn escaped(units: &[u32]) -> String {
    units
        .iter()
        .map(|unit| format!("{BACKSLASH}u{unit:04x}"))
        .collect()
}
/// The leaf metadata document a variant carries.
fn metadata(variant: Variant) -> Vec<u8> {
    use Variant as V;
    let record = br#"{"sans":["maintain.localhost"],"issuer_data":null}"#;
    let with_issuer = |issuer: &str| {
        format!(r#"{{"sans":["maintain.localhost"],"issuer_data":{issuer}}}"#).into_bytes()
    };
    let pair = |key: &str, other: &str| format!(r#"{{"{key}":0,"{other}":0}}"#).into_bytes();
    let point = |value: u32| char::from_u32(value).unwrap().to_string();
    match variant {
        V::MetadataNoIssuer => br#"{"sans": ["maintain.localhost"]}"#.to_vec(),
        V::MetadataIssuerObject => with_issuer("{}"),
        V::MetadataTwoSans | V::SchemaAndChainCount => {
            br#"{"sans": ["maintain.localhost", "other.localhost"], "issuer_data": null}"#.to_vec()
        }
        V::MetadataExtraKey => {
            br#"{"sans": ["maintain.localhost"], "issuer_data": null, "extra": null}"#.to_vec()
        }
        V::MetadataList => b"[]".to_vec(),
        V::MetadataDuplicateSans => br#"{"sans":[],"sans":[],"issuer_data":null}"#.to_vec(),
        V::MetadataNan => br#"{"sans":NaN,"issuer_data":null}"#.to_vec(),
        V::MetadataInvalidUtf8 | V::MetadataAndRootUnreadable | V::OversizedRootInvalidMetadata => {
            b"\xff".to_vec()
        }
        V::MetadataUnclosedObjects => b"{".repeat(10_000),
        V::MetadataDuplicateIssuer => {
            br#"{"sans":["maintain.localhost"],"issuer_data":null,"issuer_data":null}"#.to_vec()
        }
        V::MetadataIssuerNan => with_issuer("NaN"),
        V::MetadataNestedDuplicate => with_issuer(r#"{"a":1,"a":1}"#),
        V::MetadataEscapedDuplicate => format!(
            r#"{{"sans":[],"s{}ns":[],"issuer_data":null}}"#,
            escaped(&[0x61])
        )
        .into_bytes(),
        V::MetadataSurrogatePairDuplicate => pair(&escaped(&[0xd83d, 0xde00]), &point(0x1f600)),
        V::MetadataDuplicateTrailing => br#"{"sans":[],"sans":[],"issuer_data":null} x"#.to_vec(),
        V::MetadataDuplicateBeforeSyntax => {
            br#"{"sans":["maintain.localhost"],"sans":["maintain.localhost"],}"#.to_vec()
        }
        V::MetadataTrailingData => [&record[..], b" x"].concat(),
        V::MetadataByteOrderMark => [&b"\xef\xbb\xbf"[..], record].concat(),
        V::MetadataLoneSurrogate => format!(
            r#"{{"sans":["{}"],"issuer_data":null}}"#,
            escaped(&[0xd800])
        )
        .into_bytes(),
        V::MetadataDeepValue => with_issuer(&format!("{}{}", "[".repeat(2000), "]".repeat(2000))),
        V::MetadataHugeNumber => with_issuer("1e400"),
        V::MetadataLongestInteger => with_issuer(&"9".repeat(4300)),
        V::MetadataLongInteger => with_issuer(&"9".repeat(4301)),
        V::MetadataLongFloat => with_issuer(&format!("{}.5", "9".repeat(4301))),
        V::MetadataControlCharacter => {
            b"{\"sans\":[\"maintain.localhost\x1f\"],\"issuer_data\":null}".to_vec()
        }
        V::MetadataSpaceInName => br#"{"sans":["maintain localhost"],"issuer_data":null}"#.to_vec(),
        V::MetadataEscapes => {
            let plain = ['"', BACKSLASH, '/', 'b', 'f', 'n', 'r', 't']
                .map(|escape| format!("{BACKSLASH}{escape}"))
                .concat();
            // An upper-case high surrogate before a lower-case low one.
            let escapes = format!(
                "{plain}{}{BACKSLASH}uD834{}",
                escaped(&[0xe9]),
                escaped(&[0xdd1e])
            );
            with_issuer(&format!(
                r#"["{escapes}",true,false,null,0,-1.5e+3,2E-2,{{}}]"#
            ))
        }
        V::MetadataBadEscape => with_issuer(&format!(r#""{BACKSLASH}x41""#)),
        V::MetadataLeadingZero => with_issuer("01"),
        V::MetadataPretty => {
            b"\r\n{\t\"sans\" : [ \"maintain.localhost\" ] ,\r\n\t\"issuer_data\" : null }\n "
                .to_vec()
        }
        V::MetadataNameAsString => br#"{"sans":"maintain.localhost","issuer_data":null}"#.to_vec(),
        V::MetadataNestedName => {
            br#"{"sans":[["maintain.localhost"]],"issuer_data":null}"#.to_vec()
        }
        V::MetadataEscapedName => format!(
            r#"{{"{}ans":["maintain{}localhost"],"issuer_data":null}}"#,
            escaped(&[0x73]),
            escaped(&[0x2e])
        )
        .into_bytes(),
        V::MetadataAtBound => {
            let mut padded = record.to_vec();
            padded.resize(16_384, b' ');
            padded
        }
        V::OversizedMetadata => b" ".repeat(16_385),
        V::MetadataFractionWithoutDigits => with_issuer("1."),
        V::MetadataExponentWithoutDigits => with_issuer("1e"),
        V::MetadataExponentSignWithoutDigits => with_issuer("1e+"),
        V::MetadataMinusAlone => with_issuer("-"),
        V::MetadataFloatExponentWithoutDigits => with_issuer("1.5e"),
        V::MetadataPairLowestDuplicate => pair(&escaped(&[0xd800, 0xdc00]), &point(0x10000)),
        V::MetadataPairHighestDuplicate => pair(&escaped(&[0xdbff, 0xdfff]), &point(0x10ffff)),
        V::MetadataPairLowFirstDuplicate => pair(&escaped(&[0xd83d, 0xdc00]), &point(0x1f400)),
        V::MetadataPairLowLastDuplicate => pair(&escaped(&[0xd83d, 0xdfff]), &point(0x1f7ff)),
        V::MetadataPairBeforeLow => pair(&escaped(&[0xd83d, 0xdbff]), &escaped(&[0xd83d])),
        V::MetadataPairAfterLow => pair(&escaped(&[0xd83d, 0xe000]), &point(0x1f800)),
        _ => record.to_vec(),
    }
}
/// One PEM file with a space inside its body, a space before its first body line end, or
/// carriage returns before every line end.
fn reframed(variant: Variant, pem: Vec<u8>) -> Vec<u8> {
    use Variant as V;
    let text = String::from_utf8(pem).unwrap();
    let body = text.find('\n').unwrap() + 1;
    let first_end = body + text[body..].find('\n').unwrap();
    match variant {
        V::RootSpaceInBody | V::KeySpaceInBody => {
            format!("{} {}", &text[..body + 10], &text[body + 10..])
        }
        V::RootTrailingSpace | V::KeyTrailingSpace => {
            format!("{} {}", &text[..first_end], &text[first_end..])
        }
        _ => text.replace('\n', "\r\n"),
    }
    .into_bytes()
}
/// Replaces, removes or adds the materials a variant changes after its certificates are built.
fn edit(
    variant: Variant,
    materials: &mut BTreeMap<String, Vec<u8>>,
    [root, intermediate, leaf]: [&X509; 3],
    root_key: &PKey<Private>,
    leaf_key: &PKey<Private>,
) {
    use Variant as V;
    let pem = |certificate: &X509| certificate.to_pem().unwrap();
    let pkcs8 = |key: &PKey<Private>| key.private_key_to_pem_pkcs8().unwrap();
    let mut set = |index: usize, value: Vec<u8>| {
        materials.insert(PATHS[index].into(), value);
    };
    match variant {
        V::HeldRoot => set(4, [pem(leaf), pem(intermediate), pem(root)].concat()),
        V::ForeignRootKey
        | V::ForeignRootKeyIntermediateOcsp
        | V::ForeignRootKeyIntermediateUnsupported => set(1, pkcs8(&p256())),
        V::ForeignRootKeyBadIntermediateKey => {
            set(1, pkcs8(&p256()));
            set(3, b"inert".to_vec());
        }
        V::BadRootKeyForeignIntermediateKey => {
            set(1, b"inert".to_vec());
            set(3, pkcs8(&p256()));
        }
        V::MetadataAndRootUnreadable => set(0, b"inert".to_vec()),
        V::SchemaAndChainCount => set(4, pem(leaf)),
        V::ChainIntermediateTwice => set(4, [pem(intermediate), pem(intermediate)].concat()),
        V::ChainRootAsLeaf => set(4, [pem(root), pem(intermediate)].concat()),
        V::ChainRootAsLeafHeld => set(4, [pem(root), pem(intermediate), pem(root)].concat()),
        V::MissingAndEmptyMaterial => {
            materials.remove(PATHS[0]);
            materials.insert(PATHS[1].into(), Vec::new());
        }
        V::RootVerticalTabs => set(0, [&b"\x0b\x0c"[..], &pem(root), b"\x0b\x0c"].concat()),
        V::RootLeadingSpace => set(0, [&b" \t"[..], &pem(root)].concat()),
        V::KeyLeadingSpace => set(1, [&b" \t"[..], &pkcs8(root_key)].concat()),
        V::KeyVerticalTabs => set(
            1,
            [&b"\x0b\x0c"[..], &pkcs8(root_key), b"\x0b\x0c"].concat(),
        ),
        V::RootSpaceInBody | V::RootTrailingSpace | V::RootCrlf => {
            set(0, reframed(variant, pem(root)))
        }
        V::KeySpaceInBody | V::KeyTrailingSpace | V::KeyCrlf => {
            set(1, reframed(variant, pkcs8(root_key)));
        }
        V::ForeignIntermediateKey => set(3, pkcs8(&p256())),
        V::ForeignLeafKey | V::OcspLeafForeignLeafKey => set(5, pkcs8(&p256())),
        V::RootKeyIsLeafKey => set(1, pkcs8(leaf_key)),
        V::DoubledRootKey => set(1, [pkcs8(root_key), pkcs8(root_key)].concat()),
        V::TrailingRootText => set(0, [pem(root), b"inert trailing bytes".to_vec()].concat()),
        V::TrailingRootDer => {
            let mut der = root.to_der().unwrap();
            der.push(0);
            let block = openssl::base64::encode_block(&der);
            let text = format!("-----BEGIN CERTIFICATE-----\n{block}\n-----END CERTIFICATE-----\n");
            set(0, text.into_bytes());
        }
        V::DoubledRoot => set(0, [pem(root), pem(root)].concat()),
        V::ChainExtraLeaf => set(4, [pem(leaf), pem(intermediate), pem(leaf)].concat()),
        V::ChainReversed => set(4, [pem(intermediate), pem(leaf)].concat()),
        V::ChainForeignIntermediate => {
            let foreign = certificate(
                "fixture intermediate",
                &p256(),
                Some((root, &p256())),
                2,
                (BASE - 1800, BASE + 10 * DAY),
                Variant::Good,
            );
            set(4, [pem(leaf), pem(&foreign)].concat());
        }
        V::ChainLeafOnly => set(4, pem(leaf)),
        V::ChainSkipsIntermediate => set(4, [pem(leaf), pem(root)].concat()),
        V::FourCertificateChain => {
            set(
                4,
                [pem(leaf), pem(intermediate), pem(root), pem(root)].concat(),
            );
        }
        V::RootAsIntermediate => {
            set(2, pem(root));
            set(3, pkcs8(root_key));
            set(4, [pem(leaf), pem(root)].concat());
        }
        V::RootCertificateAtBound => {
            let mut padded = pem(root);
            padded.resize(65_536, b'\n');
            set(0, padded);
        }
        V::OversizedRootCertificate
        | V::ZeroRemainingOversizedRoot
        | V::OversizedRootInvalidMetadata => set(0, b" ".repeat(65_537)),
        V::EmptyRootCertificate => set(0, Vec::new()),
        V::BlankRootCertificate => set(0, b"\n \t\r\n".to_vec()),
        V::RenamedMaterial => {
            materials.remove(PATHS[0]);
            materials.insert("unexpected".into(), pem(root));
        }
        V::MissingRootCertificate | V::ZeroRemainingMissingRoot => {
            materials.remove(PATHS[0]);
        }
        V::ExtraMaterial => {
            materials.insert("unexpected".into(), b"inert".to_vec());
        }
        _ => {}
    }
}
/// The variant's three keys: three P-256 keys unless it names others.
fn keys(variant: Variant) -> [PKey<Private>; 3] {
    match variant {
        Variant::P521 => {
            let group = EcGroup::from_curve_name(Nid::SECP521R1).unwrap();
            let key = || PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
            [key(), key(), key()]
        }
        Variant::Rsa => {
            let rsa = PKey::from_rsa(openssl::rsa::Rsa::generate(2048).unwrap()).unwrap();
            [rsa.clone(), rsa.clone(), rsa]
        }
        Variant::Ed448 => {
            let ed448 = PKey::generate_ed448().unwrap();
            [ed448.clone(), ed448.clone(), ed448]
        }
        _ => [p256(), p256(), p256()],
    }
}
fn build(variant: Variant, keys: [PKey<Private>; 3], base: i64) -> Corpus {
    let plan = plan(variant, base);
    let [root_key, intermediate_key, leaf_key] = keys;
    let root = certificate("fixture root", &root_key, None, 1, plan.root, variant);
    let intermediate = certificate(
        "fixture intermediate",
        &intermediate_key,
        Some((&root, &root_key)),
        2,
        plan.intermediate,
        variant,
    );
    let name = if matches!(variant, Variant::WrongName) {
        "other.localhost"
    } else {
        NAME
    };
    let issuer = if matches!(variant, Variant::RootAsIntermediate) {
        (&root, &root_key)
    } else {
        (&intermediate, &intermediate_key)
    };
    let leaf = certificate(name, &leaf_key, Some(issuer), 3, plan.leaf, variant);
    let key = |key: &PKey<Private>| {
        if matches!(variant, Variant::TraditionalKeys) {
            key.ec_key().unwrap().private_key_to_pem().unwrap()
        } else {
            key.private_key_to_pem_pkcs8().unwrap()
        }
    };
    let mut chain = leaf.to_pem().unwrap();
    chain.extend(intermediate.to_pem().unwrap());
    let values = [
        root.to_pem().unwrap(),
        key(&root_key),
        intermediate.to_pem().unwrap(),
        key(&intermediate_key),
        chain,
        key(&leaf_key),
        metadata(variant),
    ];
    let mut materials = PATHS
        .into_iter()
        .zip(values)
        .map(|(p, v)| (p.into(), v))
        .collect();
    edit(
        variant,
        &mut materials,
        [&root, &intermediate, &leaf],
        &root_key,
        &leaf_key,
    );
    let now = u64::try_from(plan.now).unwrap();
    Corpus {
        materials,
        now: UNIX_EPOCH + Duration::from_micros(now),
        remaining: plan.remaining,
        root,
        intermediate,
        leaf,
        leaf_key,
    }
}
/// One variant built at the fixed clock with fresh keys.
fn built(variant: Variant) -> Corpus {
    build(variant, keys(variant), BASE)
}
fn corpus_with(variant: Variant, curve: Nid) -> Corpus {
    let group = EcGroup::from_curve_name(curve).unwrap();
    let key = || PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
    corpus_keys(variant, [key(), key(), key()])
}
/// One variant dated from the current second and inspected at the actual clock.
fn corpus_keys(variant: Variant, keys: [PKey<Private>; 3]) -> Corpus {
    let now = SystemTime::now();
    let seconds = i64::try_from(now.duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
    Corpus {
        now,
        ..build(variant, keys, seconds)
    }
}
pub(in super::super) fn corpus() -> Corpus {
    corpus_with(Variant::Good, Nid::X9_62_PRIME256V1)
}
pub(in super::super) fn wrong_name_corpus() -> Corpus {
    corpus_with(Variant::WrongName, Nid::X9_62_PRIME256V1)
}

#[test]
fn every_variant_carries_the_bytes_its_name_promises() {
    use Variant as V;
    let escape = |text: &str| format!("{BACKSLASH}{text}");
    let unit = |hex: &str| escape(&format!("u{hex}"));
    let contains = |bytes: &[u8], needle: &str| {
        bytes
            .windows(needle.len())
            .any(|window| window == needle.as_bytes())
    };
    let plain = ["\"", "\\", "/", "b", "f", "n", "r", "t"].map(escape);
    for (variant, needles) in [
        (V::MetadataEscapedDuplicate, vec![unit("0061")]),
        (V::MetadataEscapedName, vec![unit("0073"), unit("002e")]),
        (V::MetadataLoneSurrogate, vec![unit("d800")]),
        (
            V::MetadataSurrogatePairDuplicate,
            vec![unit("d83d") + &unit("de00")],
        ),
        (
            V::MetadataPairLowestDuplicate,
            vec![unit("d800") + &unit("dc00")],
        ),
        (
            V::MetadataPairHighestDuplicate,
            vec![unit("dbff") + &unit("dfff")],
        ),
        (
            V::MetadataPairLowFirstDuplicate,
            vec![unit("d83d") + &unit("dc00")],
        ),
        (
            V::MetadataPairLowLastDuplicate,
            vec![unit("d83d") + &unit("dfff")],
        ),
        (V::MetadataPairBeforeLow, vec![unit("d83d") + &unit("dbff")]),
        (V::MetadataPairAfterLow, vec![unit("d83d") + &unit("e000")]),
        (V::MetadataBadEscape, vec![escape("x41")]),
        (
            V::MetadataEscapes,
            [unit("00e9"), unit("D834") + &unit("dd1e")]
                .into_iter()
                .chain(plain.clone())
                .collect(),
        ),
    ] {
        let bytes = metadata(variant);
        for needle in needles {
            assert!(contains(&bytes, &needle), "{variant:?} lacks {needle}");
        }
    }
    // Only the variants above carry a unicode escape.
    for variant in Variant::ALL {
        let promised = format!("{variant:?}");
        if !["Escaped", "Surrogate", "Pair", "Escapes"]
            .iter()
            .any(|word| promised.contains(word))
        {
            assert!(!contains(&metadata(*variant), &escape("u")), "{promised}");
        }
    }
    assert!(metadata(V::MetadataControlCharacter).contains(&0x1f));
    assert!(metadata(V::MetadataByteOrderMark).starts_with(b"\xef\xbb\xbf"));
    assert_eq!(metadata(V::MetadataInvalidUtf8), b"\xff");
    for (variant, path, needle) in [
        (
            V::RootVerticalTabs,
            PATHS[0],
            &b"\x0b\x0c-----BEGIN CERTIFICATE-----"[..],
        ),
        (
            V::KeyVerticalTabs,
            PATHS[1],
            b"-----END PRIVATE KEY-----\n\x0b\x0c",
        ),
        (V::RootTrailingSpace, PATHS[0], b" \n"),
        (V::KeyTrailingSpace, PATHS[1], b" \n"),
        (V::RootCrlf, PATHS[0], b"-----\r\n"),
        (V::KeyCrlf, PATHS[1], b"-----\r\n"),
    ] {
        let bytes = &built(variant).materials[path];
        assert!(
            bytes.windows(needle.len()).any(|window| window == needle),
            "{variant:?}"
        );
    }
    for (variant, path) in [
        (V::RootSpaceInBody, PATHS[0]),
        (V::KeySpaceInBody, PATHS[1]),
    ] {
        let text = String::from_utf8(built(variant).materials[path].clone()).unwrap();
        let body = text.lines().nth(1).unwrap();
        assert!(body.contains(' ') && !body.ends_with(' '), "{variant:?}");
    }
}
#[test]
fn the_tls_probe_reads_at_most_three_certificates_within_the_material_bound() {
    let fixture = built(Variant::Good);
    let pem = |certificate: &X509| certificate.to_pem().unwrap();
    let three = [
        pem(&fixture.leaf),
        pem(&fixture.intermediate),
        pem(&fixture.root),
    ]
    .concat();
    assert_eq!(certificates(&three).unwrap().len(), 3);
    let four = [three, pem(&fixture.root)].concat();
    assert_eq!(certificates(&four).err(), Some(Error::Refused));
    assert_eq!(certificates(b"").err(), Some(Error::Bounds));
    let mut bound = pem(&fixture.root);
    bound.resize(65_536, b'\n');
    assert_eq!(certificates(&bound).unwrap().len(), 1);
    bound.push(b'\n');
    assert_eq!(certificates(&bound).err(), Some(Error::Bounds));
    assert_eq!(certificates(b"\n \t").err(), Some(Error::Refused));
    assert_eq!(
        certificates(b"-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n").err(),
        Some(Error::Refused)
    );
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
