"""Inspect retained natural Caddy materials without making a live TLS authority claim."""

from __future__ import annotations

import hashlib
import json
import math
import re
import ssl
from datetime import UTC, datetime, timedelta
from typing import TYPE_CHECKING, NoReturn, cast

import cryptography
from cryptography import x509
from cryptography.exceptions import InvalidSignature, UnsupportedAlgorithm
from cryptography.hazmat.primitives import serialization
from cryptography.x509.oid import (
    AuthorityInformationAccessOID,
    ExtendedKeyUsageOID,
    ExtensionOID,
    NameOID,
)

if TYPE_CHECKING:
    from collections.abc import Iterable, Mapping

type JsonValue = bool | int | float | str | list[JsonValue] | dict[str, JsonValue] | None

NAME = "maintain.localhost"
AUTHORITY = "pki/authorities/local/"
LEAF = f"certificates/local/{NAME}/{NAME}"
PATHS = (
    AUTHORITY + "root.crt",
    AUTHORITY + "root.key",
    AUTHORITY + "intermediate.crt",
    AUTHORITY + "intermediate.key",
    LEAF + ".crt",
    LEAF + ".key",
    LEAF + ".json",
)
MAX_MATERIAL = 65_536
MAX_METADATA = 16_384
MAX_TOTAL = 458_752
MIN_CRYPTOGRAPHY_MAJOR = 40
CHAIN_WITH_ROOT = 3
_CERTIFICATE = re.compile(
    rb"-----BEGIN CERTIFICATE-----[A-Za-z0-9+/=\r\n]+-----END CERTIFICATE-----"
)
_PRIVATE_KEY = re.compile(
    rb"\s*-----BEGIN (PRIVATE KEY|EC PRIVATE KEY|RSA PRIVATE KEY)-----"
    + rb"[A-Za-z0-9+/=\r\n]+-----END \1-----\s*"
)
_KNOWN_CRITICAL = frozenset(
    {
        ExtensionOID.BASIC_CONSTRAINTS,
        ExtensionOID.KEY_USAGE,
        ExtensionOID.EXTENDED_KEY_USAGE,
        ExtensionOID.SUBJECT_ALTERNATIVE_NAME,
        ExtensionOID.SUBJECT_KEY_IDENTIFIER,
        ExtensionOID.AUTHORITY_KEY_IDENTIFIER,
        ExtensionOID.AUTHORITY_INFORMATION_ACCESS,
    }
)
_UNSUPPORTED_PATH = frozenset(
    {
        ExtensionOID.NAME_CONSTRAINTS,
        ExtensionOID.POLICY_CONSTRAINTS,
        ExtensionOID.INHIBIT_ANY_POLICY,
        ExtensionOID.CERTIFICATE_POLICIES,
        ExtensionOID.POLICY_MAPPINGS,
    }
)


class MaterialError(Exception):
    """Carry a fixed public refusal code, never supplied material or parser diagnostics."""


def _refuse(code: str) -> NoReturn:
    raise MaterialError(code)


def _pairs(pairs: list[tuple[str, object]]) -> dict[str, object]:
    result: dict[str, object] = {}
    for key, value in pairs:
        if key in result:
            _refuse("metadata_duplicate")
        result[key] = value
    return result


def _constant(_value: str) -> NoReturn:
    _refuse("metadata_json")


def _metadata(raw: bytes) -> None:
    try:
        value = cast(
            "object",
            json.loads(raw.decode("utf-8"), object_pairs_hook=_pairs, parse_constant=_constant),
        )
    except (UnicodeError, ValueError, RecursionError):
        _refuse("metadata_json")
    if not isinstance(value, dict) or set(cast("dict[str, object]", value)) != {
        "sans",
        "issuer_data",
    }:
        _refuse("metadata_schema")
    metadata = cast("dict[str, object]", value)
    if metadata["sans"] != [NAME] or metadata["issuer_data"] is not None:
        _refuse("metadata_schema")


def _certificates(raw: bytes) -> list[x509.Certificate]:
    if _CERTIFICATE.sub(b"", raw).strip():
        _refuse("certificate_pem")
    try:
        result = x509.load_pem_x509_certificates(raw)
    except (ValueError, UnsupportedAlgorithm):
        _refuse("certificate_pem")
    if not result:
        _refuse("certificate_pem")
    return result


def _der(certificate: x509.Certificate) -> bytes:
    return certificate.public_bytes(serialization.Encoding.DER)


def _spki(certificate: x509.Certificate) -> bytes:
    return certificate.public_key().public_bytes(
        serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
    )


def _key_matches(certificate: x509.Certificate, raw: bytes) -> None:
    if _PRIVATE_KEY.fullmatch(raw) is None:
        _refuse("private_key_pem")
    try:
        key = serialization.load_pem_private_key(raw, password=None)
        public = key.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )
    except (ValueError, TypeError, UnsupportedAlgorithm):
        _refuse("private_key_pem")
    if public != _spki(certificate):
        _refuse("private_key_mismatch")


def _extensions(certificate: x509.Certificate) -> None:
    for extension in cast("Iterable[x509.Extension[x509.ExtensionType]]", certificate.extensions):
        if extension.oid in _UNSUPPORTED_PATH or (
            extension.critical and extension.oid not in _KNOWN_CRITICAL
        ):
            _refuse("certificate_extension")
    try:
        aia = certificate.extensions.get_extension_for_class(x509.AuthorityInformationAccess).value
    except x509.ExtensionNotFound:
        return
    descriptions = cast("Iterable[x509.AccessDescription]", aia)
    if any(item.access_method == AuthorityInformationAccessOID.OCSP for item in descriptions):
        _refuse("certificate_ocsp")


def _authority(certificate: x509.Certificate, *, root: bool) -> None:
    try:
        constraints = certificate.extensions.get_extension_for_class(x509.BasicConstraints).value
        usage = certificate.extensions.get_extension_for_class(x509.KeyUsage).value
    except x509.ExtensionNotFound:
        _refuse("authority_constraints")
    if not constraints.ca or not usage.key_cert_sign:
        _refuse("authority_constraints")
    if root and constraints.path_length is not None and constraints.path_length < 1:
        _refuse("authority_path_length")


def _subject(certificate: x509.Certificate) -> None:
    try:
        names = certificate.extensions.get_extension_for_class(x509.SubjectAlternativeName).value
        usage = certificate.extensions.get_extension_for_class(x509.KeyUsage).value
        extended = certificate.extensions.get_extension_for_class(x509.ExtendedKeyUsage).value
    except x509.ExtensionNotFound:
        _refuse("leaf_constraints")
    try:
        constraints = certificate.extensions.get_extension_for_class(x509.BasicConstraints).value
    except x509.ExtensionNotFound:
        constraints = None
    if list(names) != [x509.DNSName(NAME)]:
        _refuse("leaf_subject")
    common_names = certificate.subject.get_attributes_for_oid(NameOID.COMMON_NAME)
    if common_names and (len(common_names) != 1 or common_names[0].value != NAME):
        _refuse("leaf_subject")
    if (
        (constraints is not None and constraints.ca)
        or not usage.digital_signature
        or usage.key_cert_sign
    ):
        _refuse("leaf_constraints")
    if ExtendedKeyUsageOID.SERVER_AUTH not in extended:
        _refuse("leaf_constraints")


def _dates(certificate: x509.Certificate) -> tuple[datetime, datetime]:
    # cryptography 40 has direct issuer verification; UTC properties arrived later.
    if hasattr(certificate, "not_valid_before_utc"):
        return certificate.not_valid_before_utc, certificate.not_valid_after_utc
    return (
        certificate.not_valid_before.replace(tzinfo=UTC),
        certificate.not_valid_after.replace(tzinfo=UTC),
    )


def _facts(certificate: x509.Certificate, now: datetime, end: datetime, *, leaf: bool) -> JsonValue:
    before, after = _dates(certificate)
    if not before <= now < end < after:
        _refuse("certificate_validity")
    fraction = 2 / 3 if leaf else 0.8
    renewal = before + (after - before) * fraction
    if end >= renewal or (leaf and after - end <= timedelta(minutes=50)):
        _refuse("certificate_renewal_window")
    return {
        "certificateSha256": hashlib.sha256(_der(certificate)).hexdigest(),
        "publicKeySha256": hashlib.sha256(_spki(certificate)).hexdigest(),
        "notBefore": before.isoformat(),
        "notAfter": after.isoformat(),
        "lifetimeSeconds": (after - before).total_seconds(),
        "remainingSeconds": (after - now).total_seconds(),
        "renewalMarginAfterFixtureSeconds": (renewal - end).total_seconds(),
    }


def inspect(
    materials: Mapping[str, bytes], now_utc: datetime, remaining_seconds: float
) -> dict[str, JsonValue]:
    """Validate seven bounded byte strings; callers retain all filesystem and live TLS authority."""
    if (
        now_utc.tzinfo is None
        or now_utc.utcoffset() != timedelta(0)
        or isinstance(remaining_seconds, bool)
        or not math.isfinite(remaining_seconds)
        or remaining_seconds <= 0
    ):
        _refuse("fixture_clock")
    try:
        end = now_utc + timedelta(seconds=remaining_seconds)
        major = int(cryptography.__version__.split(".", maxsplit=1)[0])
    except (ValueError, OverflowError):
        _refuse("fixture_clock")
    if major < MIN_CRYPTOGRAPHY_MAJOR or not hasattr(x509.Certificate, "verify_directly_issued_by"):
        _refuse("analyzer_prerequisite")
    if set(materials) != set(PATHS):
        _refuse("material_set")
    if any(not value or len(value) > MAX_MATERIAL for value in materials.values()):
        _refuse("material_size")
    if (
        len(materials[LEAF + ".json"]) > MAX_METADATA
        or sum(map(len, materials.values())) > MAX_TOTAL
    ):
        _refuse("material_size")
    _metadata(materials[LEAF + ".json"])
    try:
        return _inspect_certificates(materials, now_utc, end)
    except (x509.DuplicateExtension, ValueError, UnsupportedAlgorithm):
        _refuse("certificate_structure")


def _inspect_certificates(
    materials: Mapping[str, bytes], now: datetime, end: datetime
) -> dict[str, JsonValue]:
    root_set = _certificates(materials[PATHS[0]])
    intermediate_set = _certificates(materials[PATHS[2]])
    chain = _certificates(materials[PATHS[4]])
    if len(root_set) != 1 or len(intermediate_set) != 1 or len(chain) not in {2, 3}:
        _refuse("certificate_chain")
    root, intermediate, leaf = root_set[0], intermediate_set[0], chain[0]
    expected = [leaf, intermediate] + ([root] if len(chain) == CHAIN_WITH_ROOT else [])
    if list(map(_der, chain)) != list(map(_der, expected)):
        _refuse("certificate_chain")
    certificates = (root, intermediate, leaf)
    if len({_der(certificate) for certificate in certificates}) != len(certificates):
        _refuse("certificate_chain")
    for certificate, path in zip(certificates, (PATHS[1], PATHS[3], PATHS[5]), strict=True):
        _extensions(certificate)
        _key_matches(certificate, materials[path])
    _authority(root, root=True)
    _authority(intermediate, root=False)
    _subject(leaf)
    try:
        root.verify_directly_issued_by(root)
        intermediate.verify_directly_issued_by(root)
        leaf.verify_directly_issued_by(intermediate)
    except (ValueError, TypeError, InvalidSignature, UnsupportedAlgorithm):
        _refuse("certificate_signature")
    return {
        "schemaVersion": 1,
        "name": NAME,
        "analysis": "retained-materials-only",
        "cryptographyVersion": cryptography.__version__,
        "sslVersion": ssl.OPENSSL_VERSION,
        "chainCertificateCount": len(chain),
        "metadataSansCount": 1,
        "metadataIssuerDataNull": True,
        "ocspUriCount": 0,
        "root": _facts(root, now, end, leaf=False),
        "intermediate": _facts(intermediate, now, end, leaf=False),
        "leaf": _facts(leaf, now, end, leaf=True),
    }
