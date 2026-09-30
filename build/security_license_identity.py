"""Keep raw license review identity stable across equivalent image-layer rebuilds.

Only the verified SHA-256 at a Syft license location's ``layerID`` is replaced.
The field's presence, every other field and all array ordering remain part of
the review identity. Full original records retain a separate provenance hash.
"""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass

from security_tools import require

# isort: split
from release_json import JsonValue, array_value, object_value, string_value

LAYER_IDENTITY = "sha256:<image-layer>"


@dataclass(frozen=True)
class RawLicenseIdentity:
    """A semantic review fingerprint and an unchanged scanner-evidence digest."""

    fingerprint: str
    records_sha256: str


def digest(value: JsonValue) -> str:
    """Hash complete ordered JSON records with deterministic object-key encoding."""
    return hashlib.sha256(
        json.dumps(
            value, sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False
        ).encode()
    ).hexdigest()


def raw_license_identity(records: list[JsonValue]) -> RawLicenseIdentity:
    """Retain all declared semantics while separating validated layer provenance."""
    normalized: list[JsonValue] = []
    for item in records:
        record = object_value(item).copy()
        if "locations" in record:
            locations: list[JsonValue] = []
            for value in array_value(record["locations"]):
                location = object_value(value).copy()
                if "layerID" in location:
                    require(
                        re.fullmatch(r"sha256:[a-f0-9]{64}", string_value(location["layerID"])),
                        "image_license_layer_identity",
                    )
                    location["layerID"] = LAYER_IDENTITY
                locations.append(location)
            record["locations"] = locations
        normalized.append(record)
    return RawLicenseIdentity(
        fingerprint="license-raw-v2:" + digest(normalized),
        records_sha256=digest(records),
    )
