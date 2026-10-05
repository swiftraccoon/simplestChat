"""Exercise runtime selection checks without contacting a daemon or host."""

import io
import json
import subprocess
import sys
import unittest
from collections.abc import Sequence
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

from test_support import obj, strings, yaml_value

TASKS = Path(__file__).resolve().parents[1] / "tasks"


def inline_code(filename: str) -> str:
    """Extract the maintained first task's Python verifier."""
    task = obj(yaml_value((TASKS / filename).read_text()), 0)
    return strings(task, "ansible.builtin.command", "argv")[2]


class RuntimeComponentTests(unittest.TestCase):
    """The live selection must match every pinned runtime component."""

    def test_runtime_versions_reject_each_old_or_missing_component(self) -> None:
        """Wrong client or server versions cannot pass the same production check."""
        code = inline_code("runtime-verify.yml")
        for mismatch in ("none", "engine", "containerd", "runc", "compose", "buildx", "command"):

            def response(
                args: Sequence[str], *, selected: str = mismatch, **_options: object
            ) -> subprocess.CompletedProcess[bytes]:
                if "--short" in args:
                    value = b"5.5.1" if selected == "compose" else b"5.6.0"
                elif "buildx" in args:
                    value = (
                        b"github.com/docker/buildx v0.37.1"
                        if selected == "buildx"
                        else b"github.com/docker/buildx v0.37.2 revision"
                    )
                else:
                    value = json.dumps(
                        {
                            "Version": "29.7.2" if selected == "engine" else "29.8.2",
                            "Components": [
                                {
                                    "Name": "containerd",
                                    "Version": "v2.3.6" if selected == "containerd" else "v2.4.1",
                                },
                                {
                                    "Name": "runc",
                                    "Version": "1.5.1" if selected == "runc" else "1.5.2",
                                },
                            ],
                        }
                    ).encode()
                return subprocess.CompletedProcess(
                    args, 1 if selected == "command" else 0, value, b""
                )

            with (
                self.subTest(mismatch=mismatch),
                patch.object(
                    sys, "argv", ["verify", "29.8.2", "5.6.0", "2.4.1", "0.37.2", "1.5.2"]
                ),
                patch.object(subprocess, "run", side_effect=response),
                redirect_stdout(io.StringIO()),
            ):
                if mismatch == "none":
                    exec(compile(code, "runtime-verify", "exec"), {})  # noqa: S102 -- Execute the checked-in verifier with isolated command doubles.
                else:
                    with self.assertRaises(SystemExit):
                        exec(compile(code, "runtime-verify", "exec"), {})  # noqa: S102 -- No host command is executed.

    def test_containerd_service_refuses_custom_units_overrides_and_arguments(self) -> None:
        """Only the packaged service and the one managed selection are accepted."""
        code = inline_code("runtime-binaries.yml")
        for selection in ("package", "managed", "unit", "dropin", "arguments", "unknown"):
            executable = (
                "/usr/local/lib/simplestchat-runtime/2.4.1/bin/containerd"
                if selection == "managed"
                else "/usr/bin/containerd"
            )
            fields = {
                "LoadState": "not-found" if selection == "unknown" else "loaded",
                "FragmentPath": "/etc/systemd/system/containerd.service"
                if selection == "unit"
                else "/usr/lib/systemd/system/containerd.service",
                "DropInPaths": "/etc/systemd/system/containerd.service.d/custom.conf"
                if selection == "dropin"
                else "/etc/systemd/system/containerd.service.d/simplestchat-runtime.conf"
                if selection == "managed"
                else "",
                "ExecStart": "{ argv[]="
                + executable
                + (" --custom" if selection == "arguments" else "")
                + " ; }",
            }
            result = subprocess.CompletedProcess(
                ["systemctl"], 0, "\n".join(f"{key}={value}" for key, value in fields.items()), ""
            )
            with (
                self.subTest(selection=selection),
                patch.object(subprocess, "run", return_value=result),
            ):
                if selection in {"package", "managed"}:
                    exec(compile(code, "containerd-service", "exec"), {})  # noqa: S102 -- Reviewed inline code, fixture outputs only.
                else:
                    with self.assertRaises(SystemExit):
                        exec(compile(code, "containerd-service", "exec"), {})  # noqa: S102 -- No system service is contacted.


if __name__ == "__main__":
    _ = unittest.main()
