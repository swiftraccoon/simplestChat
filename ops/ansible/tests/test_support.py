"""Bootstrap maintained standalone modules for offline unittest discovery.

Tests import this module before the helpers they exercise. Installed VPS helpers
remain flat modules; neither production packaging nor the installed host's code
is imported by the test suite. Keep these paths aligned with pyproject.toml.
"""

import sys
from pathlib import Path
from typing import cast

import yaml

ROOT = Path(__file__).resolve().parents[3]
for directory in ("build", "ops/ansible/files", "ops/ansible/callback_plugins"):
    sys.path.insert(0, str(ROOT / directory))

# isort: split
# Flat host helpers are available only after the checkout bootstrap above.
from release_json import (  # noqa: E402
    JsonObject,
    JsonValue,
    array_value,
    json_value,
    object_value,
    string_value,
)


def yaml_value(text: str, *, scalars_as_strings: bool = False) -> JsonValue:
    """Load configuration as validated JSON-shaped data, never arbitrary Python objects."""
    loader = yaml.BaseLoader if scalars_as_strings else yaml.SafeLoader
    # Both selected loaders prohibit Python constructors; BaseLoader preserves
    # GitHub's YAML 1.2 `on` key and deliberately leaves all scalars as strings.
    return json_value(cast("object", yaml.load(text, Loader=loader)))  # noqa: S506


def at(value: JsonValue, *path: str | int) -> JsonValue:
    """Read a required path, checking the container kind at every component."""
    for component in path:
        value = (
            object_value(value)[component]
            if isinstance(component, str)
            else array_value(value)[component]
        )
    return value


def obj(value: JsonValue, *path: str | int) -> JsonObject:
    """Read a required object with a checked path and result kind."""
    return object_value(at(value, *path))


def array(value: JsonValue, *path: str | int) -> list[JsonValue]:
    """Read a required array with a checked path and result kind."""
    return array_value(at(value, *path))


def string(value: JsonValue, *path: str | int) -> str:
    """Read a required string without coercion."""
    return string_value(at(value, *path))


def objects(value: JsonValue, *path: str | int) -> list[JsonObject]:
    """Read an array whose elements must all be configuration objects."""
    return [object_value(item) for item in array(value, *path)]


def strings(value: JsonValue, *path: str | int) -> list[str]:
    """Read an array whose elements must all be strings."""
    return [string_value(item) for item in array(value, *path)]
