"""Validate untrusted JSON without leaking dynamically typed values to callers.

The release utilities use only the Python standard library on the host. These
helpers validate structure; callers still enforce their own schema, size limits,
identities and error-redaction rules. Objects with duplicate keys and non-finite
numbers are rejected instead of silently choosing an ambiguous interpretation.
"""

import json
import math
from typing import cast

type JsonValue = bool | int | float | str | list[JsonValue] | dict[str, JsonValue] | None
type JsonObject = dict[str, JsonValue]


class DuplicateJsonError(ValueError):
    """An object contains repeated field names and cannot identify one value."""

    def __init__(self, key: str) -> None:
        """Retain the repeated field name for adapters with an existing error contract."""
        self.key: str = key
        super().__init__(f"Duplicate JSON key: {key}")


def json_value(value: object) -> JsonValue:
    """Copy a decoded JSON/YAML value into the validated recursive value domain."""
    if value is None or isinstance(value, (str, bool, int)):
        return value
    if isinstance(value, float) and math.isfinite(value):
        return value
    if isinstance(value, list):
        # Only the container type is asserted; every element is checked below.
        return [json_value(item) for item in cast("list[object]", value)]
    if isinstance(value, dict):
        result: JsonObject = {}
        # Key and value types remain unknown until individually validated.
        for key, item in cast("dict[object, object]", value).items():
            if not isinstance(key, str):
                message = "JSON object keys must be strings"
                raise ValueError(message)  # noqa: TRY004 - malformed wire values are validation errors.
            result[key] = json_value(item)
        return result
    message = "Unsupported JSON value"
    raise ValueError(message)


def _pairs(pairs: list[tuple[str, JsonValue]]) -> JsonObject:
    result: JsonObject = {}
    for key, value in pairs:
        if key in result:
            raise DuplicateJsonError(key)
        result[key] = value
    return result


def decode_json(data: str | bytes) -> JsonValue:
    """Decode JSON and validate all values, including duplicate object keys."""
    return json_value(cast("object", json.loads(data, object_pairs_hook=_pairs)))


def object_value(value: JsonValue) -> JsonObject:
    """Require an object before accessing schema fields."""
    if isinstance(value, dict):
        return value
    message = "Expected a JSON object"
    raise ValueError(message)


def array_value(value: JsonValue) -> list[JsonValue]:
    """Require an array before iterating its elements."""
    if isinstance(value, list):
        return value
    message = "Expected a JSON array"
    raise ValueError(message)


def string_value(value: JsonValue) -> str:
    """Require a string without coercing numbers, booleans or null."""
    if isinstance(value, str):
        return value
    message = "Expected a JSON string"
    raise ValueError(message)


def integer_value(value: JsonValue) -> int:
    """Require an integer, explicitly excluding Python's boolean subclass."""
    if isinstance(value, int) and not isinstance(value, bool):
        return value
    message = "Expected a JSON integer"
    raise ValueError(message)


def boolean_value(value: JsonValue) -> bool:
    """Require a boolean without accepting truthy strings or numbers."""
    if isinstance(value, bool):
        return value
    message = "Expected a JSON boolean"
    raise ValueError(message)
