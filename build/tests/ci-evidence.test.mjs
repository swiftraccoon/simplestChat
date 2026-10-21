import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import {
  copyFileSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const repository = fileURLToPath(new URL("../../", import.meta.url));
execFileSync("cargo", ["build", "--locked", "-q", "-p", "xtask"], {
  cwd: repository,
  stdio: "inherit",
});
const binary = path.join(repository, "target/debug/xtask");
const gates = [
  "automation",
  "rust-lint",
  "rust-test",
  "native-dtls",
  "browser",
  "web",
  "image-database",
  "deployment",
  "security",
  "codeql",
];
const checks = [
  "browser-accounts",
  "browser-media",
  "browser-stress",
  "security-fast",
  "codeql-actions",
  "codeql-javascript-typescript",
  "codeql-python",
  "codeql-rust",
];

function fixture(t) {
  const root = realpathSync(
    mkdtempSync(path.join(tmpdir(), "simplestchat-evidence-")),
  );
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const filename of [
    ".github/workflows/ci.yml",
    ".github/workflows/security.yml",
    ".github/workflows/codeql.yml",
    "security/codeql-toolchain.json",
  ]) {
    mkdirSync(path.dirname(path.join(root, filename)), { recursive: true });
    copyFileSync(path.join(repository, filename), path.join(root, filename));
  }
  const evidence = path.join(root, "evidence");
  writeFileSync(
    path.join(root, "event.json"),
    JSON.stringify({ before: "b".repeat(40) }),
  );
  const env = {
    ...process.env,
    ACT: "true",
    LOCAL_CI_DISPOSABLE: "1",
    LOCAL_CI_RUN_ID: "1".repeat(32),
    GITHUB_SHA: "a".repeat(40),
    GITHUB_EVENT_PATH: path.join(root, "event.json"),
    LOCAL_CI_EVIDENCE: evidence,
    GATE_RESULTS: JSON.stringify(
      Object.fromEntries(gates.map((name) => [name, { result: "success" }])),
    ),
  };
  return {
    evidence,
    run: (args, overrides = {}) =>
      spawnSync(binary, ["ci-evidence", ...args, "--root", root], {
        cwd: repository,
        encoding: "utf8",
        env: { ...env, ...overrides },
      }),
  };
}

test("local evidence CLI requires its owned runner and validates invocation before publishing", (t) => {
  const f = fixture(t);
  assert.equal(f.run([]).status, 2);
  assert.equal(f.run(["invented"]).status, 2);
  assert.equal(f.run(["record", "--id"]).status, 2);
  const unowned = f.run(["record", "--id", "browser-media"], { ACT: "" });
  assert.equal(unowned.status, 1);
  assert.match(
    unowned.stderr,
    /Local CI evidence failed: local_ci_runner_only/,
  );
  const invalid = f.run(["record", "--id", "../escape"]);
  assert.equal(invalid.status, 1);
  assert.match(invalid.stderr, /local_ci_check_id/);
});

test("local evidence CLI records all real matrices and completes exactly once", (t) => {
  const f = fixture(t);
  for (const check of checks) {
    const result = f.run(["record", "--id", check]);
    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, "");
  }
  const duplicate = f.run(["record", "--id", checks[0]]);
  assert.equal(duplicate.status, 1);
  const result = f.run(["complete"]);
  assert.equal(result.status, 0, result.stderr);
  const filename = path.join(f.evidence, "required.json");
  const report = JSON.parse(readFileSync(filename, "utf8"));
  assert.deepEqual(report.checks, [...checks].sort());
  assert.deepEqual(report.gates, [...gates].sort());
  assert.equal(report.runId, "1".repeat(32));
  assert.equal(report.revision, "a".repeat(40));
  assert.equal(report.base, "b".repeat(40));
  assert.equal(statSync(filename).mode & 0o777, 0o600);
  assert.equal(f.run(["complete"]).status, 1);
});

test("local evidence workflow calls the compiled producer with no Python YAML bootstrap", () => {
  const ci = readFileSync(
    path.join(repository, ".github/workflows/ci.yml"),
    "utf8",
  );
  const security = readFileSync(
    path.join(repository, ".github/workflows/security.yml"),
    "utf8",
  );
  const codeql = readFileSync(
    path.join(repository, ".github/workflows/codeql.yml"),
    "utf8",
  );
  for (const source of [ci, security, codeql]) {
    assert.doesNotMatch(
      source,
      /build\/ci-local-evidence\.py|local-ci-yaml|local-ci-evidence-tools/,
    );
    assert.match(
      source,
      /cargo run --locked -q -p xtask -- ci-evidence record --id/,
    );
  }
  assert.match(ci, /cargo run --locked -q -p xtask -- ci-evidence complete/);
  for (const source of [ci.slice(ci.indexOf("\n  required:")), codeql]) {
    assert.match(
      source,
      /Install the pinned Rust toolchain for local execution receipts\n\s+if: \$\{\{ env\.ACT \}\}\n\s+uses: dtolnay\/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d/,
    );
  }
});
