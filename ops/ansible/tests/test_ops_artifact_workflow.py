"""Keep hosted Ops production and its exact signer separate from server release."""

from __future__ import annotations

import unittest

from test_support import ROOT, obj, objects, string, yaml_value

CHECKOUT = "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1"
RUST = "dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d"
UPLOAD = "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"
DOWNLOAD = "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"
ATTEST = "actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6"
GUARD = "${{ github.event_name == 'push' && github.ref == 'refs/heads/main' }}"
TARGET = "x86_64-unknown-linux-musl"
FIXED = (
    "simplestchat-ops",
    "ops-release.json",
    "ops-evidence.json",
    "ops-predicate.json",
    "ops-preparation.json",
    *(f"ops-evidence-{index:02}.bin" for index in range(9)),
)


class HostedOpsWorkflowTests(unittest.TestCase):
    """Inspect actual workflow structure; no action, candidate or tool runs."""

    def test_build_has_no_signing_authority_or_cache_success_shortcut(self) -> None:
        """Run the fixed owner with authenticated materials on each main push."""
        jobs = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True),
            "jobs",
        )
        job = obj(jobs, "ops-build")
        self.assertEqual(job["if"], GUARD)
        self.assertEqual(job["runs-on"], "ubuntu-24.04")
        self.assertEqual(job["timeout-minutes"], "65")
        self.assertEqual(job["permissions"], {"contents": "read"})
        self.assertNotIn("needs", job)
        steps = objects(job, "steps")
        self.assertEqual(len(steps), 4)
        self.assertEqual(steps[0]["uses"], CHECKOUT)
        self.assertEqual(obj(steps[0], "with")["persist-credentials"], "false")
        self.assertEqual(steps[1]["uses"], RUST)
        self.assertEqual(steps[1]["with"], {"toolchain": "1.99.0"})
        command = "cargo run --locked -q -p xtask -- ops-artifact hosted"
        self.assertEqual(steps[2]["run"], f'{command} --output "$RUNNER_TEMP/ops-hosted"')
        self.assertEqual(steps[3]["uses"], UPLOAD)
        upload = obj(steps[3], "with")
        self.assertEqual(
            upload["name"], f"simplestchat-ops-unsigned-${{{{ github.sha }}}}-{TARGET}"
        )
        self.assertEqual(
            string(upload, "path").splitlines(),
            [f"${{{{ runner.temp }}}}/ops-hosted/unsigned/{name}" for name in FIXED],
        )
        self.assertEqual(upload["if-no-files-found"], "error")
        self.assertEqual(upload["compression-level"], "0")
        self.assertEqual(upload["retention-days"], "14")
        for step in steps:
            self.assertNotIn("if", step)
            self.assertNotIn("continue-on-error", step)
            self.assertNotIn("env", step)

    def test_signer_binds_three_original_subjects_and_only_one_flat_payload_graph(self) -> None:
        """No wildcard, third-party signer or self-verification can enter the job."""
        jobs = obj(
            yaml_value((ROOT / ".github/workflows/ci.yml").read_text(), scalars_as_strings=True),
            "jobs",
        )
        job = obj(jobs, "ops-release-security")
        self.assertEqual(job["if"], GUARD)
        self.assertEqual(job["needs"], ["required", "ops-build"])
        self.assertEqual(job["runs-on"], "ubuntu-24.04")
        self.assertEqual(job["timeout-minutes"], "10")
        self.assertEqual(
            job["permissions"],
            {"contents": "read", "id-token": "write", "attestations": "write"},
        )
        steps = objects(job, "steps")
        self.assertEqual(len(steps), 7)
        self.assertEqual(steps[0]["uses"], CHECKOUT)
        self.assertEqual(obj(steps[0], "with")["persist-credentials"], "false")
        self.assertEqual(steps[1]["uses"], RUST)
        self.assertEqual(steps[1]["with"], {"toolchain": "1.99.0"})
        self.assertEqual(steps[2]["uses"], DOWNLOAD)
        self.assertEqual(
            steps[2]["with"],
            {
                "name": f"simplestchat-ops-unsigned-${{{{ github.sha }}}}-{TARGET}",
                "path": "${{ runner.temp }}/ops-unsigned",
            },
        )
        admission = "cargo run --locked -q -p xtask -- ops-artifact signing-inputs"
        directories = '--directory "$RUNNER_TEMP/ops-unsigned" --output "$RUNNER_TEMP/ops-signed"'
        self.assertEqual(steps[3]["run"], f"{admission} {directories}")
        self.assertEqual(steps[4]["uses"], ATTEST)
        self.assertEqual(steps[4]["id"], "ops-attest")
        self.assertEqual(
            obj(steps[4], "with"),
            {
                "subject-path": "\n".join(
                    f"${{{{ runner.temp }}}}/ops-signed/{name}"
                    for name in ("ops-preparation.json", "ops-release.json", "simplestchat-ops")
                )
                + "\n",
                "predicate-path": "${{ runner.temp }}/ops-signed/ops-predicate.json",
                "predicate-type": (
                    "https://github.com/swiftraccoon/simplestChat/attestations/ops-artifact/v1"
                ),
                "create-storage-record": "false",
            },
        )
        self.assertEqual(
            steps[5]["env"],
            {"ATTESTATION_BUNDLE": "${{ steps.ops-attest.outputs.bundle-path }}"},
        )
        bundle_copy = 'install -m 600 -- "$ATTESTATION_BUNDLE"'
        self.assertEqual(
            steps[5]["run"],
            f'{bundle_copy} "$RUNNER_TEMP/ops-signed/ops-attestation.jsonl"',
        )
        self.assertEqual(steps[6]["uses"], UPLOAD)
        upload = obj(steps[6], "with")
        self.assertEqual(upload["name"], f"simplestchat-ops-${{{{ github.sha }}}}-{TARGET}")
        self.assertEqual(
            string(upload, "path").splitlines(),
            [
                f"${{{{ runner.temp }}}}/ops-signed/{name}"
                for name in (*FIXED, "ops-attestation.jsonl")
            ],
        )
        self.assertEqual(upload["if-no-files-found"], "error")
        self.assertEqual(upload["compression-level"], "0")
        self.assertEqual(upload["retention-days"], "14")
        self.assertEqual(
            obj(jobs, "required")["needs"],
            [
                "automation",
                "automation-references",
                "rust-lint",
                "rust-test",
                "native-dtls",
                "browser",
                "web",
                "image-database",
                "deployment",
                "security",
                "codeql",
            ],
        )
        server = obj(jobs, "release-security")
        self.assertEqual(server["needs"], ["required", "release-prepare"])
        server_steps = objects(server, "steps")
        predicates = [obj(step, "with") for step in server_steps if step.get("uses") == ATTEST]
        self.assertEqual(len(predicates), 1)
        self.assertEqual(
            predicates[0]["predicate-type"],
            "https://github.com/swiftraccoon/simplestChat/attestations/release/v1",
        )
