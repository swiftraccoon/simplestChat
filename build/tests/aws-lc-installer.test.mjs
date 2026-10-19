import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  symlink,
  writeFile,
} from "node:fs/promises";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../../", import.meta.url));
const installer = join(root, "build/install-aws-lc.sh");

async function temporary(t) {
  await mkdir(join(root, "target"), { recursive: true });
  const directory = await mkdtemp(
    join(root, "target", "aws-lc-installer-test-"),
  );
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

function run(prefix, env = {}) {
  return spawnSync("/bin/sh", [installer, prefix], {
    cwd: root,
    encoding: "utf8",
    timeout: 10000,
    env: { ...process.env, ...env },
  });
}

test("changed release archive fails authentication before extraction or compilation", async (t) => {
  const directory = await temporary(t);
  const bin = join(directory, "bin");
  await mkdir(bin);
  await writeFile(
    join(bin, "curl"),
    `#!/bin/sh
while [ "$#" -gt 0 ]; do
  if [ "$1" = --output ]; then shift; printf 'untrusted source archive' > "$1"; exit 0; fi
  shift
done
exit 9
`,
    { mode: 0o755 },
  );
  await writeFile(join(bin, "cargo"), "#!/bin/sh\nexit 77\n", { mode: 0o755 });
  const result = run(join(directory, "prefix"), {
    PATH: `${bin}:${process.env.PATH}`,
  });
  assert.equal(result.status, 1, result.stderr);
  assert.match(result.stderr, /SHA-256 mismatch/);
  assert.deepEqual(await readdir(directory), ["bin"]);
});

test("failed download removes its private staging and leaves no install", async (t) => {
  const directory = await temporary(t);
  const bin = join(directory, "bin");
  await mkdir(bin);
  await writeFile(join(bin, "curl"), "#!/bin/sh\nexit 22\n", { mode: 0o755 });
  const result = run(join(directory, "prefix"), {
    PATH: `${bin}:${process.env.PATH}`,
  });
  assert.equal(result.status, 22, result.stderr);
  assert.deepEqual(await readdir(directory), ["bin"]);
});

async function receiptedPrefix(directory, installerDigest) {
  const prefix = join(directory, "prefix");
  const files = {
    "lib/libcrypto-awslc.a": "archive",
    "share/rust/aws_lc_bindings.rs": "bindings",
  };
  const digests = {};
  for (const [name, contents] of Object.entries(files)) {
    await mkdir(join(prefix, name, ".."), { recursive: true });
    await writeFile(join(prefix, name), contents);
    digests[name] = createHash("sha256").update(contents).digest("hex");
  }
  await mkdir(join(prefix, "share/simplestchat"), { recursive: true });
  await writeFile(
    join(prefix, "share/simplestchat/build.json"),
    JSON.stringify({ installer_sha256: installerDigest, files: digests }),
  );
  return prefix;
}

test("a complete installation by this installer is retained", async (t) => {
  const directory = await temporary(t);
  const bin = join(directory, "bin");
  await mkdir(bin);
  // Any download attempt would fail loudly: retention must not reach one.
  await writeFile(join(bin, "curl"), "#!/bin/sh\nexit 22\n", { mode: 0o755 });
  const digest = createHash("sha256")
    .update(await readFile(installer))
    .digest("hex");
  const prefix = await receiptedPrefix(directory, digest);
  const result = run(prefix, { PATH: `${bin}:${process.env.PATH}` });
  assert.equal(result.status, 0, result.stderr);
  assert.match(result.stderr, /already installed by this installer/);
  assert.equal(
    await readFile(join(prefix, "lib/libcrypto-awslc.a"), "utf8"),
    "archive",
  );
});

test("another installer's or a damaged installation is replaced only after a successful build", async (t) => {
  const directory = await temporary(t);
  const bin = join(directory, "bin");
  await mkdir(bin);
  await writeFile(join(bin, "curl"), "#!/bin/sh\nexit 22\n", { mode: 0o755 });
  const prefix = await receiptedPrefix(directory, "0".repeat(64));
  const result = run(prefix, { PATH: `${bin}:${process.env.PATH}` });
  assert.equal(result.status, 22, result.stderr);
  assert.match(result.stderr, /Replacing the AWS-LC installation/);
  assert.equal(
    await readFile(join(prefix, "lib/libcrypto-awslc.a"), "utf8"),
    "archive",
  );
  const digest = createHash("sha256")
    .update(await readFile(installer))
    .digest("hex");
  const damaged = await receiptedPrefix(directory, digest);
  await writeFile(join(damaged, "lib/libcrypto-awslc.a"), "truncated");
  const second = run(damaged, { PATH: `${bin}:${process.env.PATH}` });
  assert.equal(second.status, 22, second.stderr);
  assert.match(second.stderr, /Replacing the AWS-LC installation/);
});

test("existing directory and symlink destinations are never replaced", async (t) => {
  const directory = await temporary(t);
  const prefix = join(directory, "prefix");
  await mkdir(prefix);
  await writeFile(join(prefix, "retained"), "original");
  const link = join(directory, "link");
  await symlink(prefix, link);
  for (const destination of [prefix, link]) {
    const result = run(destination);
    assert.equal(result.status, 2, result.stderr);
    assert.match(result.stderr, /destination (already exists|is a symbolic link)/);
  }
  assert.equal(await readFile(join(prefix, "retained"), "utf8"), "original");
  assert.deepEqual((await readdir(directory)).sort(), ["link", "prefix"]);
});

test("global destinations, relative prefixes and invalid parallelism fail closed", async (t) => {
  const directory = await temporary(t);
  for (const prefix of ["/", "/usr", "/usr/local", "/opt", "relative"]) {
    assert.equal(run(prefix).status, 2);
  }
  for (const jobs of ["0", "-1", "65", "nonsense"]) {
    assert.equal(
      run(join(directory, "prefix"), { AWS_LC_BUILD_JOBS: jobs }).status,
      2,
    );
    assert.deepEqual(await readdir(directory), []);
  }
});
