import assert from "node:assert/strict";
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
    assert.match(result.stderr, /destination already exists/);
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
