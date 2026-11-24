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

// The installation this installer leaves: its eight recorded files, the CMake
// cache it read the options from, and its receipt in the installer's own form.
async function receiptedPrefix(directory, installerDigest) {
  const prefix = join(directory, "prefix");
  const options = {
    CMAKE_BUILD_TYPE: "Release",
    BUILD_SHARED_LIBS: "OFF",
    BUILD_TESTING: "OFF",
    BUILD_TOOL: "OFF",
    BUILD_LIBSSL: "OFF",
    ENABLE_SOURCE_MODIFICATION: "OFF",
    BUILD_AWSLC_PROVIDER: "OFF",
    ENABLE_CRYPTO_POLICIES: "OFF",
    GENERATE_RUST_BINDINGS: "ON",
    RUST_BINDINGS_TARGET_VERSION: "1.70",
    BORINGSSL_PREFIX: "simplestchat_awslc_5_11_0",
    CMAKE_POSITION_INDEPENDENT_CODE: "ON",
    CMAKE_INSTALL_LIBDIR: "lib",
  };
  const cache = Object.entries(options)
    .map(([name, value]) => `//${name} help\n${name}:STRING=${value}\n`)
    .join("");
  const files = {
    "lib/libcrypto-awslc.a": "archive",
    "share/rust/aws_lc_bindings.rs": "bindings",
    "include/openssl/base.h": "header",
    "include/openssl/boringssl_prefix_symbols.h": "symbols header",
    "share/simplestchat/aws-lc-source.tar.gz": "source",
    "share/simplestchat/bindgen-cli.crate": "generator",
    "share/simplestchat/CMakeCache.txt": `# CMake cache\n${cache}`,
    "share/simplestchat/symbols.txt": "symbols",
  };
  const digests = {};
  for (const [name, contents] of Object.entries(files)) {
    await mkdir(join(prefix, name, ".."), { recursive: true });
    await writeFile(join(prefix, name), contents);
    digests[name] = createHash("sha256").update(contents).digest("hex");
  }
  const receipt = {
    configure_options: options,
    installer_sha256: installerDigest,
    bindgen_sha256: "1".repeat(64),
    files: digests,
  };
  await writeFile(
    join(prefix, "share/simplestchat/build.json"),
    `${JSON.stringify(receipt, null, 2)}\n`,
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
