"""Keep reviewed workflow locations bound to the exact source bytes of their review."""

from __future__ import annotations

import hashlib
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, cast

import yaml
from test_support import at, yaml_value
from yaml.nodes import MappingNode, Node, SequenceNode

# isort: split
import security_policy

if TYPE_CHECKING:
    from collections.abc import Callable

ROOT = Path(__file__).resolve().parents[3]


class SecurityWorkflowTests(unittest.TestCase):
    """Require each reviewed workflow feature to be the one that was reviewed."""

    def reviewed_feature(self, text: str, components: list[str | int]) -> str:
        """Preserve scalar features and exact source spelling for reviewed YAML blocks."""
        self.assertLessEqual(len(text.encode()), security_policy.MAX_POLICY)
        document = yaml_value(text, scalars_as_strings=True)
        selected = at(document, *components)
        if isinstance(selected, str):
            return selected
        self.assertIsInstance(selected, dict)
        compose = cast("Callable[[str], Node | None]", yaml.compose)
        node = compose(text)
        start = 0
        for component in components:
            if isinstance(component, int):
                self.assertIsInstance(node, SequenceNode)
                node = cast("list[Node]", cast("SequenceNode", node).value)[component]
                start = node.start_mark.index
            else:
                self.assertIsInstance(node, MappingNode)
                matches = [
                    (key, value)
                    for key, value in cast(
                        "list[tuple[Node, Node]]", cast("MappingNode", node).value
                    )
                    if cast("object", key.value) == component
                ]
                self.assertEqual(len(matches), 1)
                key, node = matches[0]
                start = key.start_mark.index
        self.assertIsInstance(node, MappingNode)
        return text[start : cast("MappingNode", node).end_mark.index].rstrip()

    def test_reviewed_mapping_preserves_source_bytes_and_detects_nested_changes(self) -> None:
        """A reviewed trigger includes its key and nested values, without a final newline."""
        feature = (
            "on:\n  workflow_run:\n    workflows: [CI]\n"
            "    types: [completed]\n    branches: [main]"
        )
        text = "name: Cache retention\n\n" + feature + "\n\npermissions:\n  contents: read\n"
        self.assertEqual(self.reviewed_feature(text, ["on"]), feature)
        self.assertNotEqual(
            self.reviewed_feature(text.replace("[main]", "[other]"), ["on"]), feature
        )
        action = "owner/action@" + "a" * 40
        self.assertEqual(
            self.reviewed_feature(
                "steps:\n  - uses: " + action + " # pinned\n", ["steps", 0, "uses"]
            ),
            action,
        )

    def test_reviewed_workflow_locations_still_identify_the_same_feature(self) -> None:
        """Moving a reviewed action must update its exact review location before CI runs."""
        reviews = [
            entry for entry in security_policy.read_exceptions() if entry.scanner == "zizmor"
        ]
        self.assertTrue(reviews)
        for review in reviews:
            with self.subTest(scope=review.scope):
                name, route = review.scope.split("#", 1)
                text = (ROOT / name).read_text()
                components = [int(part) if part.isdecimal() else part for part in route.split("/")]
                feature = self.reviewed_feature(text, components)
                self.assertEqual(
                    review.fingerprint.split(":", 1)[1],
                    hashlib.sha256(feature.encode()).hexdigest(),
                )


if __name__ == "__main__":
    _ = unittest.main()
