"""Execute retained original checker/tests with audited inert transport only.

The unused YAML/template import dependencies are inert sentinels. Their APIs are
not exercised by these fifteen tests; invoking rendering would fail. The actual
retained checker, harness, test modules and local dependencies keep their source
filenames and original relative layout. No process or socket can be created.
"""

from __future__ import annotations

import contextlib
import importlib
import io
import json
import sys
import types
import unittest
from pathlib import Path
from typing import TYPE_CHECKING, Never, Protocol, cast
from unittest.mock import patch

if TYPE_CHECKING:
    from collections.abc import Callable, Sequence

sys.dont_write_bytecode = True
REFERENCE = Path(__file__).resolve().parent / "reference"
MAX_INPUT = 4 * 1024 * 1024
MAX_CASES = 512
ORIGINAL_TESTS = 15
HEADER_PAIR_LENGTH = 2


def refuse(code: str) -> Never:
    """Raise only the caller's fixed oracle diagnostic."""
    raise RuntimeError(code)


def denied(*_args: object, **_kwargs: object) -> object:
    """Refuse every unused YAML/template operation."""
    refuse("unused_dependency_api")


class UnusedDumper:
    """Allow only the original import-time representer registration."""

    @classmethod
    def add_representer(cls, value: type, callback: object) -> None:
        """Register no renderer and refuse any unrelated import action."""
        if value.__name__ != "QuotedImage" or getattr(callback, "__name__", "") != "_quoted_image":
            refuse("unused_dependency_api")


class Endpoint(Protocol):
    """The selected original endpoint fields used by the fixture."""

    path: str
    status: int
    content_type: str
    limit: int
    no_store: bool


class Connection(Protocol):
    """The original inert connection's independently observed final state."""

    closed: bool


class TestModule(Protocol):
    """Only the real retained test module's exercised fixture APIs."""

    CADDY: str
    FakeResponse: Callable[[Endpoint, dict[str, str]], object]
    FakeConnection: Callable[[object], Connection]

    def fixture(self) -> object:
        """Construct the original synthetic fixture identity."""
        ...

    def headers(self, endpoint: Endpoint) -> dict[str, str]:
        """Return the original complete security headers."""
        ...


class PolicyModule(Protocol):
    """The exact original policy entry points invoked through dynamic module loading."""

    ENDPOINTS: tuple[Endpoint, ...]
    PolicyError: type[Exception]
    bounded_process: object
    socket: object
    http: HttpModule

    def header_map(self, pairs: list[tuple[str, str]]) -> dict[str, str]:
        """Run the actual duplicate/control/size header policy."""
        ...

    def validate_response(
        self, endpoint: Endpoint, status: int, headers: dict[str, str], body: bytes
    ) -> None:
        """Run all unchanged body and response policy checks."""
        ...

    def owned_fixture(self, fixture: object) -> None:
        """Check the selected fixture through inert inspection responses."""
        ...

    def request(self, endpoint: Endpoint, context: object) -> object:
        """Run the original request over its own inert response and connection classes."""
        ...

    def success_report(self) -> dict[str, object]:
        """Return the original fixed public success receipt."""
        ...


class HttpModule(Protocol):
    """The original HTTP module patched only at the connection constructor boundary."""

    client: object


def audit(event: str, _arguments: tuple[object, ...]) -> None:
    """Prohibit real subprocesses and sockets even if a fixture accidentally falls through."""
    if event.startswith(("subprocess.", "socket.", "os.exec", "os.spawn")) or event in {
        "os.system",
        "os.fork",
        "os.posix_spawn",
        "pty.spawn",
    }:
        refuse("oracle_external_action_refused")


def setup() -> tuple[types.ModuleType, PolicyModule]:
    """Import the complete retained local dependency closure with original file origins."""
    sys.addaudithook(audit)
    yaml = types.ModuleType("yaml")
    yaml.__dict__.update(
        {
            "SafeDumper": UnusedDumper,
            "SafeLoader": object,
            "BaseLoader": object,
            "load": denied,
            "safe_load": denied,
            "dump": denied,
        }
    )
    sys.modules["yaml"] = yaml
    template = types.ModuleType("jinja2")
    template.__dict__.update({"Environment": denied, "StrictUndefined": object})
    sys.modules["jinja2"] = template
    sys.path[:0] = [
        str(REFERENCE / "ops/ansible/tests"),
        str(REFERENCE / "build"),
        str(REFERENCE / "ops/ansible/files"),
    ]
    tests = importlib.import_module("test_release_http_policy")
    policy = importlib.import_module("release_http_policy")
    for name in (
        "release_http_policy",
        "release_container_harness",
        "release_container_fixture",
        "bounded_process",
        "release_json",
        "release_artifact",
        "release_assets",
        "release_public",
        "runtime_profile",
        "test_release_http_policy",
        "test_release_container_harness",
        "test_support",
    ):
        filename = sys.modules[name].__file__
        if filename is None:
            refuse("oracle_origin")
        origin = Path(filename).resolve()
        if not origin.is_relative_to(REFERENCE):
            refuse("oracle_origin")
    return tests, cast("PolicyModule", cast("object", policy))


def run_original_tests(tests: types.ModuleType) -> None:
    """Run all actual fifteen tests without rewriting assertions or their helpers."""
    output = io.StringIO()
    with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
        suite = unittest.defaultTestLoader.loadTestsFromModule(tests)
        result = unittest.TextTestRunner(stream=output).run(suite)
    if result.testsRun != ORIGINAL_TESTS or not result.wasSuccessful():
        refuse("original_tests_failed: " + output.getvalue())


def sequence(value: object) -> list[object]:
    """Validate a JSON array before assigning its elements to a fixed case projection."""
    if not isinstance(value, list):
        message = "oracle_case_array"
        raise TypeError(message)
    return cast("list[object]", value)


def integer(value: object) -> int:
    """Validate an original case's integer input without a permissive numeric cast."""
    if not isinstance(value, int):
        message = "oracle_case_integer"
        raise TypeError(message)
    return value


def text(value: object) -> str:
    """Validate an original case's selected text input."""
    if not isinstance(value, str):
        message = "oracle_case_text"
        raise TypeError(message)
    return value


def object_value(value: object) -> dict[str, object]:
    """Keep dynamic JSON values private until a case explicitly selects their fields."""
    if not isinstance(value, dict):
        message = "oracle_case_object"
        raise TypeError(message)
    return cast("dict[str, object]", value)


def header_pairs(value: object) -> list[tuple[str, str]]:
    """Retain order and duplicates in the original wire-header case representation."""
    pairs: list[tuple[str, str]] = []
    for row in sequence(value):
        pair = sequence(row)
        if len(pair) != HEADER_PAIR_LENGTH:
            message = "oracle_case_pair"
            raise ValueError(message)
        pairs.append((text(pair[0]), text(pair[1])))
    return pairs


def run_case(case: dict[str, object], tests: TestModule, policy: PolicyModule) -> None:
    """Invoke actual original policy/transport functions over bounded inert case inputs."""
    kind = case["kind"]
    if kind == "response":
        endpoint = policy.ENDPOINTS[integer(case["index"])]
        values = policy.header_map(header_pairs(case["headers"]))
        policy.validate_response(
            endpoint,
            integer(case["status"]),
            values,
            bytes(integer(v) for v in sequence(case["body"])),
        )
    elif kind == "headers":
        _ = policy.header_map(header_pairs(case["headers"]))
    elif kind == "ownership":
        records = iter(sequence(case["records"]))

        def inspect(_identity: str) -> object:
            return next(records)

        with patch.object(policy, "inspect_container", side_effect=inspect):
            policy.owned_fixture(tests.fixture())
    elif kind == "inspection":
        observations = sequence(case["records"])

        def run(args: Sequence[str], **_kwargs: object) -> tuple[int, str, str]:
            return 0, text(observations[0 if args[-1] == tests.CADDY else 1]), ""

        with patch.object(policy.bounded_process, "run", side_effect=run):
            policy.owned_fixture(tests.fixture())
    elif kind == "length":
        endpoint = policy.ENDPOINTS[integer(case["index"])]
        response = tests.FakeResponse(
            endpoint, {**tests.headers(endpoint), "content-length": text(case["value"])}
        )
        connection = tests.FakeConnection(response)

        def wrap_socket(*_args: object, **_kwargs: object) -> object:
            return object()

        context = types.SimpleNamespace(wrap_socket=wrap_socket)
        with (
            patch.object(policy.socket, "create_connection", return_value=object()),
            patch.object(policy.http.client, "HTTPConnection", return_value=connection),
        ):
            _ = policy.request(endpoint, context)
        if not connection.closed:
            refuse("oracle_connection_close")
    else:
        refuse("oracle_kind")


def main() -> None:
    """Bound input/output and export only fixed verdicts from the original functions."""
    raw = sys.stdin.buffer.read(MAX_INPUT + 1)
    if len(raw) > MAX_INPUT:
        refuse("oracle_input")
    request = object_value(cast("object", json.loads(raw)))
    if set(request) != {"cases"} or not isinstance(request["cases"], list):
        refuse("oracle_input")
    cases = sequence(cast("object", request["cases"]))
    if len(cases) > MAX_CASES:
        refuse("oracle_input")
    tests, policy = setup()
    run_original_tests(tests)
    rows: list[dict[str, object]] = []
    for case in cases:
        try:
            run_case(object_value(case), cast("TestModule", cast("object", tests)), policy)
            rows.append({"passed": True})
        except policy.PolicyError as error:
            rows.append({"passed": False, "code": str(error)})
        except (ValueError, KeyError, TypeError, OSError):
            rows.append({"passed": False, "code": "http_policy_failed"})
    json.dump(
        {"originalTests": ORIGINAL_TESTS, "report": policy.success_report(), "results": rows},
        sys.stdout,
    )
    _ = sys.stdout.write("\n")


if __name__ == "__main__":
    main()
