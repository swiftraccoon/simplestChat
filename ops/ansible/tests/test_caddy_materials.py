"""Exercise passive material admission with synthetic certificates and no live TLS peer."""

from __future__ import annotations

import ipaddress
import json
import unittest
from dataclasses import dataclass, replace
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import final, override

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import AuthorityInformationAccessOID, ExtendedKeyUsageOID, NameOID
from test_support import ROOT

# isort: split
import caddy_materials as analyzer

NOW = datetime(2050, 1, 1, tzinfo=UTC)
REMAINING = 1_336.0


def equal(observed: object, expected: object) -> None:
    """Keep failed fixture checks bounded without printing material values."""
    if observed != expected:
        message = "synthetic fixture comparison failed"
        raise AssertionError(message)


@dataclass(frozen=True)
class Policy:
    """Describe one small synthetic certificate, separate from real bootstrap authority."""

    ca: bool = False
    basic_constraints: bool = True
    key_usage: bool = True
    digital_signature: bool = True
    client_auth: bool = False
    path_length: int | None = None
    lifetime: timedelta = timedelta(hours=12)
    names: tuple[x509.GeneralName, ...] = (x509.DNSName(analyzer.NAME),)
    server_auth: bool = True
    sign_certificates: bool = False
    ocsp: bool = False
    critical_unknown: bool = False
    constraints: bool = False


def signed(
    subject: x509.Name,
    issuer: x509.Name,
    key: ec.EllipticCurvePrivateKey,
    signer: ec.EllipticCurvePrivateKey,
    policy: Policy,
) -> x509.Certificate:
    """Use mature builders and signatures for a closed synthetic chain."""
    builder = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(NOW - timedelta(seconds=10))
        .not_valid_after(NOW - timedelta(seconds=10) + policy.lifetime)
    )
    if policy.basic_constraints:
        builder = builder.add_extension(
            x509.BasicConstraints(policy.ca, policy.path_length), critical=True
        )
    if policy.key_usage:
        builder = builder.add_extension(
            x509.KeyUsage(
                digital_signature=policy.digital_signature,
                content_commitment=False,
                key_encipherment=False,
                data_encipherment=False,
                key_agreement=False,
                key_cert_sign=policy.sign_certificates,
                crl_sign=policy.sign_certificates,
                encipher_only=False,
                decipher_only=False,
            ),
            critical=True,
        )
    builder = builder.add_extension(
        x509.SubjectKeyIdentifier.from_public_key(key.public_key()), critical=False
    ).add_extension(
        x509.AuthorityKeyIdentifier.from_issuer_public_key(signer.public_key()), critical=False
    )
    if policy.names:
        builder = builder.add_extension(x509.SubjectAlternativeName(policy.names), critical=False)
    if policy.server_auth or policy.client_auth:
        purposes = ([ExtendedKeyUsageOID.SERVER_AUTH] if policy.server_auth else []) + (
            [ExtendedKeyUsageOID.CLIENT_AUTH] if policy.client_auth else []
        )
        builder = builder.add_extension(x509.ExtendedKeyUsage(purposes), critical=False)
    if policy.ocsp:
        builder = builder.add_extension(
            x509.AuthorityInformationAccess(
                [
                    x509.AccessDescription(
                        AuthorityInformationAccessOID.OCSP,
                        x509.UniformResourceIdentifier("https://inert.invalid/never-contact"),
                    )
                ]
            ),
            critical=False,
        )
    if policy.critical_unknown:
        builder = builder.add_extension(
            x509.UnrecognizedExtension(x509.ObjectIdentifier("1.2.3.4.5"), b"inert"), critical=True
        )
    if policy.constraints:
        builder = builder.add_extension(
            x509.NameConstraints([x509.DNSName(analyzer.NAME)], None), critical=False
        )
    return builder.sign(signer, hashes.SHA256())


def pem(certificate: x509.Certificate) -> bytes:
    """Encode a synthetic certificate."""
    return certificate.public_bytes(serialization.Encoding.PEM)


def private(key: ec.EllipticCurvePrivateKey) -> bytes:
    """Encode a test-only private key without exposing it in diagnostics."""
    return key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )


@final
class Fixture:
    """Own an entirely in-memory generated chain with the natural seven-file layout."""

    def __init__(self) -> None:
        """Generate independent synthetic authority, intermediate, and leaf keys."""
        self.root_key = ec.generate_private_key(ec.SECP256R1())
        self.intermediate_key = ec.generate_private_key(ec.SECP256R1())
        self.leaf_key = ec.generate_private_key(ec.SECP256R1())
        self.root_name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Inert test root")])
        self.intermediate_name = x509.Name(
            [x509.NameAttribute(NameOID.COMMON_NAME, "Inert test intermediate")]
        )
        self.root_policy = Policy(
            ca=True,
            path_length=1,
            lifetime=timedelta(days=3_600),
            names=(),
            server_auth=False,
            sign_certificates=True,
        )
        self.intermediate_policy = replace(
            self.root_policy, path_length=0, lifetime=timedelta(days=7)
        )
        self.root = signed(
            self.root_name, self.root_name, self.root_key, self.root_key, self.root_policy
        )
        self.intermediate = signed(
            self.intermediate_name,
            self.root_name,
            self.intermediate_key,
            self.root_key,
            self.intermediate_policy,
        )
        self.leaf = self.make_leaf(Policy())

    def make_leaf(self, policy: Policy, common_name: str | None = None) -> x509.Certificate:
        """Sign a leaf, optionally exercising the source's optional CN semantics."""
        subject = x509.Name(
            [] if common_name is None else [x509.NameAttribute(NameOID.COMMON_NAME, common_name)]
        )
        return signed(subject, self.intermediate_name, self.leaf_key, self.intermediate_key, policy)

    def materials(self, leaf: x509.Certificate | None = None) -> dict[str, bytes]:
        """Return fresh byte mappings so mutations cannot pollute another case."""
        return dict(
            zip(
                analyzer.PATHS,
                (
                    pem(self.root),
                    private(self.root_key),
                    pem(self.intermediate),
                    private(self.intermediate_key),
                    pem(self.leaf if leaf is None else leaf) + pem(self.intermediate),
                    private(self.leaf_key),
                    json.dumps({"sans": [analyzer.NAME], "issuer_data": None}).encode(),
                ),
                strict=True,
            )
        )


@final
class MaterialTests(unittest.TestCase):
    """Check the closed certificate profile and sanitized failure boundary."""

    @override
    def __init__(self, methodName: str = "runTest") -> None:
        """Use unrelated fresh keys for every synthetic test."""
        super().__init__(methodName)
        self.fixture = Fixture()

    def refuses(self, materials: dict[str, bytes], code: str, *, now: datetime = NOW) -> None:
        """Require the exact fixed refusal with no parser or material disclosure."""
        try:
            _ = analyzer.inspect(materials, now, REMAINING)
        except analyzer.MaterialError as error:
            equal(str(error), code)
        else:
            message = "synthetic negative fixture was admitted"
            raise AssertionError(message)

    @classmethod
    @override
    def setUpClass(cls) -> None:
        """Require this suite to load the maintained module rather than a foreign flat import."""
        expected = ROOT / "build/caddy_materials.py"
        if Path(analyzer.__file__).resolve() != expected:
            message = "maintained_caddy_module_source"
            raise AssertionError(message)

    def test_normal_twelve_hour_leaf_and_optional_held_root(self) -> None:
        """Admit the natural short-lived leaf, both source-permitted chain encodings."""
        materials = self.fixture.materials()
        facts = analyzer.inspect(materials, NOW, REMAINING)
        equal(facts["chainCertificateCount"], 2)
        equal(facts["analysis"], "retained-materials-only")
        leaf = facts["leaf"]
        if not isinstance(leaf, dict):
            message = "missing closed leaf facts"
            raise TypeError(message)
        equal(leaf["lifetimeSeconds"], 43_200)
        materials[analyzer.PATHS[4]] += pem(self.fixture.root)
        equal(analyzer.inspect(materials, NOW, REMAINING)["chainCertificateCount"], 3)

    def test_leaf_without_basic_constraints_has_source_supported_non_ca_profile(self) -> None:
        """Sign real bytes without BC; retain exact SAN, KU/EKU and both chain encodings."""
        leaf = self.fixture.make_leaf(replace(Policy(), basic_constraints=False))
        with self.assertRaises(x509.ExtensionNotFound):
            _ = leaf.extensions.get_extension_for_class(x509.BasicConstraints)
        materials = self.fixture.materials(leaf)
        equal(analyzer.inspect(materials, NOW, REMAINING)["chainCertificateCount"], 2)
        materials[analyzer.PATHS[4]] += pem(self.fixture.root)
        equal(analyzer.inspect(materials, NOW, REMAINING)["chainCertificateCount"], 3)

    def test_optional_leaf_bc_never_weakens_ca_or_mandatory_usage(self) -> None:
        """A trusted issuer's signature cannot replace non-CA state or signing/server purpose."""
        base = replace(Policy(), basic_constraints=False)
        for policy in (
            replace(Policy(), ca=True),
            replace(base, names=()),
            replace(base, digital_signature=False),
            replace(base, sign_certificates=True),
            replace(base, key_usage=False),
            replace(base, server_auth=False),
            replace(base, server_auth=False, client_auth=True),
        ):
            with self.subTest(policy=policy):
                self.refuses(
                    self.fixture.materials(self.fixture.make_leaf(policy)), "leaf_constraints"
                )

    def test_root_and_intermediate_still_require_explicit_basic_constraints(self) -> None:
        """Only the leaf gets optional BC; neither actual signing authority can omit it."""
        root = signed(
            self.fixture.root_name,
            self.fixture.root_name,
            self.fixture.root_key,
            self.fixture.root_key,
            replace(self.fixture.root_policy, basic_constraints=False),
        )
        materials = self.fixture.materials()
        materials[analyzer.PATHS[0]] = pem(root)
        self.refuses(materials, "authority_constraints")
        intermediate = signed(
            self.fixture.intermediate_name,
            self.fixture.root_name,
            self.fixture.intermediate_key,
            self.fixture.root_key,
            replace(self.fixture.intermediate_policy, basic_constraints=False),
        )
        materials = self.fixture.materials()
        materials[analyzer.PATHS[2]] = pem(intermediate)
        materials[analyzer.PATHS[4]] = pem(self.fixture.leaf) + pem(intermediate)
        self.refuses(materials, "authority_constraints")

    def test_output_contains_only_public_certificate_facts(self) -> None:
        """Public output contains no private PEM, supplied metadata, subjects, or live proof."""
        facts = analyzer.inspect(self.fixture.materials(), NOW, REMAINING)
        encoded = json.dumps(facts)
        for forbidden in ("PRIVATE KEY", "Inert test", "issuer_data", "https://", '"passed"'):
            equal(forbidden in encoded, expected=False)

    def test_wrong_private_key_is_refused(self) -> None:
        """All three private/public bindings are checked independently."""
        for index in (1, 3, 5):
            with self.subTest(index=index):
                materials = self.fixture.materials()
                materials[analyzer.PATHS[index]] = private(ec.generate_private_key(ec.SECP256R1()))
                self.refuses(materials, "private_key_mismatch")

    def test_extra_key_or_certificate_bytes_are_refused(self) -> None:
        """Mature parsers do not authorize ignored trailing material."""
        for index, suffix, code in (
            (1, private(self.fixture.root_key), "private_key_pem"),
            (0, b"inert trailing bytes", "certificate_pem"),
            (4, pem(self.fixture.leaf), "certificate_chain"),
            (0, pem(self.fixture.root), "certificate_chain"),
        ):
            with self.subTest(index=index, code=code):
                materials = self.fixture.materials()
                materials[analyzer.PATHS[index]] += suffix
                self.refuses(materials, code)

    def test_chain_order_and_foreign_intermediate_are_refused(self) -> None:
        """Only the exact held authority chain may accompany the leaf."""
        for chain in (
            pem(self.fixture.intermediate) + pem(self.fixture.leaf),
            pem(self.fixture.leaf) + pem(Fixture().intermediate),
            pem(self.fixture.leaf),
        ):
            materials = self.fixture.materials()
            materials[analyzer.PATHS[4]] = chain
            self.refuses(materials, "certificate_chain")

    def test_pkcs8_private_keys_are_admitted_without_parser_fallback(self) -> None:
        """Both ordinary Caddy-compatible unencrypted key encodings use mature parsing."""
        materials = self.fixture.materials()
        for index, key in zip(
            (1, 3, 5),
            (self.fixture.root_key, self.fixture.intermediate_key, self.fixture.leaf_key),
            strict=True,
        ):
            materials[analyzer.PATHS[index]] = key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            )
        equal(analyzer.inspect(materials, NOW, REMAINING)["chainCertificateCount"], 2)

    def test_duplicate_root_cannot_supply_the_intermediate_path(self) -> None:
        """A repeated self-signed certificate cannot stand in for the intended two-CA path."""
        leaf = signed(
            x509.Name([]),
            self.fixture.root_name,
            self.fixture.leaf_key,
            self.fixture.root_key,
            Policy(),
        )
        materials = self.fixture.materials(leaf)
        materials[analyzer.PATHS[2]] = pem(self.fixture.root)
        materials[analyzer.PATHS[3]] = private(self.fixture.root_key)
        materials[analyzer.PATHS[4]] = pem(leaf) + pem(self.fixture.root)
        self.refuses(materials, "certificate_chain")

    def test_signature_verification_uses_actual_issuer_key(self) -> None:
        """Matching issuer names and subject keys cannot replace signature verification."""
        leaf = signed(
            x509.Name([]),
            self.fixture.intermediate_name,
            self.fixture.leaf_key,
            ec.generate_private_key(ec.SECP256R1()),
            Policy(),
        )
        self.refuses(self.fixture.materials(leaf), "certificate_signature")

    def test_root_path_length_and_authority_usage_are_refused(self) -> None:
        """Enforce the actual two-CA path rather than signature-only success."""
        for policy, code in (
            (replace(self.fixture.root_policy, path_length=0), "authority_path_length"),
            (replace(self.fixture.root_policy, sign_certificates=False), "authority_constraints"),
        ):
            root = signed(
                self.fixture.root_name,
                self.fixture.root_name,
                self.fixture.root_key,
                self.fixture.root_key,
                policy,
            )
            materials = self.fixture.materials()
            materials[analyzer.PATHS[0]] = pem(root)
            self.refuses(materials, code)

    def test_root_and_intermediate_signatures_are_checked_independently(self) -> None:
        """Forged signatures fail even with every held key, name, and path unchanged."""
        foreign = ec.generate_private_key(ec.SECP256R1())
        root = signed(
            self.fixture.root_name,
            self.fixture.root_name,
            self.fixture.root_key,
            foreign,
            self.fixture.root_policy,
        )
        materials = self.fixture.materials()
        materials[analyzer.PATHS[0]] = pem(root)
        self.refuses(materials, "certificate_signature")
        intermediate = signed(
            self.fixture.intermediate_name,
            self.fixture.root_name,
            self.fixture.intermediate_key,
            foreign,
            self.fixture.intermediate_policy,
        )
        materials = self.fixture.materials()
        materials[analyzer.PATHS[2]] = pem(intermediate)
        materials[analyzer.PATHS[4]] = pem(self.fixture.leaf) + pem(intermediate)
        self.refuses(materials, "certificate_signature")

    def test_extra_dns_ip_uri_and_wrong_common_name_are_refused(self) -> None:
        """The only subject capability is the exact fixed DNS name."""
        for extra in (
            x509.DNSName("other.localhost"),
            x509.DNSName("*.localhost"),
            x509.IPAddress(ipaddress.IPv4Address("127.0.0.1")),
            x509.RFC822Name("inert@example.invalid"),
            x509.UniformResourceIdentifier("https://inert.invalid"),
        ):
            leaf = self.fixture.make_leaf(
                replace(Policy(), names=(x509.DNSName(analyzer.NAME), extra))
            )
            self.refuses(self.fixture.materials(leaf), "leaf_subject")
        leaf = self.fixture.make_leaf(Policy(), "other.localhost")
        self.refuses(self.fixture.materials(leaf), "leaf_subject")
        leaf = self.fixture.make_leaf(Policy(), analyzer.NAME)
        equal(analyzer.inspect(self.fixture.materials(leaf), NOW, REMAINING)["name"], analyzer.NAME)

    def test_leaf_ca_usage_and_server_auth_are_required(self) -> None:
        """A certificate signed by the right issuer still needs leaf constraints."""
        for policy in (
            replace(Policy(), ca=True),
            replace(Policy(), sign_certificates=True),
            replace(Policy(), server_auth=False),
        ):
            self.refuses(self.fixture.materials(self.fixture.make_leaf(policy)), "leaf_constraints")

    def test_ocsp_unknown_critical_and_unhandled_path_constraints_are_refused(self) -> None:
        """Reject unsupported network and path semantics without claiming generic WebPKI."""
        for policy, code in (
            (replace(Policy(), ocsp=True), "certificate_ocsp"),
            (replace(Policy(), critical_unknown=True), "certificate_extension"),
            (replace(Policy(), constraints=True), "certificate_extension"),
        ):
            self.refuses(self.fixture.materials(self.fixture.make_leaf(policy)), code)

    def test_metadata_requires_exact_source_schema(self) -> None:
        """Issuer null and sole SAN are actual source data, not guessed issuer paths."""
        variants: list[analyzer.JsonValue] = [
            {"sans": [analyzer.NAME]},
            {"sans": [analyzer.NAME], "issuer_data": {}},
            {"sans": [analyzer.NAME, "other.localhost"], "issuer_data": None},
            {"sans": [analyzer.NAME], "issuer_data": None, "extra": None},
            [],
        ]
        for value in variants:
            materials = self.fixture.materials()
            materials[analyzer.PATHS[6]] = json.dumps(value).encode()
            self.refuses(materials, "metadata_schema")

    def test_metadata_duplicate_and_invalid_json_are_refused(self) -> None:
        """Reject ambiguous encodings and nonstandard JSON constants."""
        for raw, code in (
            (b'{"sans":[],"sans":[],"issuer_data":null}', "metadata_duplicate"),
            (b'{"sans":NaN,"issuer_data":null}', "metadata_json"),
            (b"\xff", "metadata_json"),
            (b"{" * 10_000, "metadata_json"),
        ):
            materials = self.fixture.materials()
            materials[analyzer.PATHS[6]] = raw
            self.refuses(materials, code)

    def test_expired_not_yet_valid_and_fixture_window_are_refused(self) -> None:
        """All admitted certificate dates must cover the complete remaining fixture."""
        self.refuses(
            self.fixture.materials(), "certificate_validity", now=NOW - timedelta(minutes=1)
        )
        self.refuses(self.fixture.materials(), "certificate_validity", now=NOW + timedelta(days=1))
        leaf = self.fixture.make_leaf(replace(Policy(), lifetime=timedelta(minutes=10)))
        self.refuses(self.fixture.materials(leaf), "certificate_validity")

    def test_leaf_and_intermediate_renewal_windows_are_refused(self) -> None:
        """Do not begin a fixture that would enter natural renewal before completion."""
        self.refuses(
            self.fixture.materials(), "certificate_renewal_window", now=NOW + timedelta(hours=8)
        )
        leaf = self.fixture.make_leaf(replace(Policy(), lifetime=timedelta(hours=1)))
        self.refuses(self.fixture.materials(leaf), "certificate_renewal_window")
        intermediate = signed(
            self.fixture.intermediate_name,
            self.fixture.root_name,
            self.fixture.intermediate_key,
            self.fixture.root_key,
            replace(self.fixture.intermediate_policy, lifetime=timedelta(minutes=25)),
        )
        materials = self.fixture.materials()
        materials[analyzer.PATHS[2]] = pem(intermediate)
        materials[analyzer.PATHS[4]] = pem(self.fixture.leaf) + pem(intermediate)
        self.refuses(materials, "certificate_renewal_window")

    def test_material_set_and_all_byte_bounds_are_refused(self) -> None:
        """Bound raw metadata and parser inputs before any certificate work."""
        materials = self.fixture.materials()
        del materials[analyzer.PATHS[0]]
        self.refuses(materials, "material_set")
        materials = self.fixture.materials()
        materials["unexpected"] = b"inert"
        self.refuses(materials, "material_set")
        for index, length in (
            (0, 0),
            (0, analyzer.MAX_MATERIAL + 1),
            (6, analyzer.MAX_METADATA + 1),
        ):
            materials = self.fixture.materials()
            materials[analyzer.PATHS[index]] = b" " * length
            self.refuses(materials, "material_size")

    def test_invalid_clock_inputs_have_fixed_refusals(self) -> None:
        """No new clock or infinite fixture interval can be smuggled into analysis."""
        for seconds in (0.0, -1.0, float("inf"), float("nan"), 1e300, True):
            try:
                _ = analyzer.inspect(self.fixture.materials(), NOW, seconds)
            except analyzer.MaterialError as error:
                equal(str(error), "fixture_clock")
            else:
                message = "invalid fixture clock admitted"
                raise AssertionError(message)
        self.refuses(self.fixture.materials(), "fixture_clock", now=NOW.replace(tzinfo=None))


if __name__ == "__main__":
    _ = unittest.main()
