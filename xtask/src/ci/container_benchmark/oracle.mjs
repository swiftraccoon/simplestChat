// Execute original tests and extracted original validators, never either launcher.
import assert from "node:assert/strict";
import { spawnSync as realSpawn } from "node:child_process";
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const root = path.dirname(fileURLToPath(import.meta.url));
const references = [
  [
    "reference/build/benchmark-container.sh",
    16168,
    "a329d731b73630b5823e4a2d626178858b1adc305e1be2972b3d8b8b90edf5de",
  ],
  [
    "reference/build/cleanup-container-benchmark.sh",
    5725,
    "f6a5fffd69fd1cb8edbfda3ba669c41a973f1d78de462b6cd27cbfc2c760f83e",
  ],
  [
    "reference/build/tests/container-benchmark.test.mjs",
    16799,
    "590341ce46218f81f1a2d1c86ca32231cfd3f4f82b5569f861981f99be530a3b",
  ],
];
const sources = references.map(([name, length, hash]) => {
  const bytes = fs.readFileSync(path.join(root, name));
  assert.equal(bytes.length, length);
  assert.equal(createHash("sha256").update(bytes).digest("hex"), hash);
  return bytes.toString("utf8");
});
const [runner, cleanup, testsSource] = sources;
const extract = (source, expression) => {
  const match = source.match(expression);
  assert.ok(match);
  return match[1];
};
const summaryFilter = extract(
  runner,
  /'([.]schemaVersion == 2[\s\S]*?)' "\$summary"/,
);
const countFilter = extract(
  runner,
  /zero_counts\(\) \{\s+awk '([\s\S]*?)' "\$1"\s+\}/,
);
const ownershipFilter = extract(
  cleanup,
  /'(\.id == \$id and \.image == \$image[^']*)'/,
);
const exitFilter = extract(
  runner,
  /jq -e '(\.Running == false and \.ExitCode == 0[^']*)'/,
);
const bounded = extract(runner, /^(bounded\(\) \{[^\n]+\})$/m);
const publicGuard = extract(
  runner,
  /^(require_public_chat_stopped\(\) \{[\s\S]*?^\})$/m,
);
const emergency = extract(
  cleanup,
  /(if \[\[ "\$mode" == --emergency \]\]; then\n  retained_result=''[\s\S]*?\nfi)\nfor role/,
);
const boundedScript = `${bounded}\nbounded "$@"`;
const publicScript = `set -eu
fixture_containers=$1
fixture_status=$2
docker_owned() {
  [[ $# == 4 && $1 == ps && $2 == --quiet && $3 == --filter && $4 == label=com.docker.compose.project=simplestchat-public ]] || return 97
  printf '%s' "$fixture_containers"
  return "$fixture_status"
}
${publicGuard}
require_public_chat_stopped`;
const emergencyScript = (
  preparation,
) => `set -eu\numask 077\noutput=$1\nrun_id=$2\nmode=--emergency
cleanup_ok=true\n${preparation}\n${emergency}\nprintf '%s' "$cleanup_ok"`;
const environment = { PATH: process.env.PATH, LC_ALL: "C", LANG: "C" };
const tools = Object.fromEntries(
  ["jq", "awk", "bash"].map((name) => {
    for (const directory of (process.env.PATH ?? "").split(path.delimiter)) {
      if (!path.isAbsolute(directory)) continue;
      const candidate = path.join(directory, name);
      try {
        if (fs.statSync(candidate).isFile()) {
          fs.accessSync(candidate, fs.constants.X_OK);
          return [name, candidate];
        }
      } catch {
        /* only try the next caller-selected executable */
      }
    }
    throw new Error("Missing inert reference tool.");
  }),
);
const owned = new Set();
function admitted(value) {
  const name = value instanceof URL ? fileURLToPath(value) : String(value);
  assert.ok(path.isAbsolute(name));
  assert.ok(
    [...owned].some(
      (directory) =>
        name === directory || name.startsWith(`${directory}${path.sep}`),
    ),
  );
  return name;
}
function temporary() {
  const directory = fs.mkdtempSync(
    path.join(os.tmpdir(), "simplestchat-emergency-evidence."),
  );
  owned.add(directory);
  return directory;
}
function remove(directory) {
  assert.ok(owned.has(directory));
  fs.rmSync(directory, { recursive: true, force: true });
  owned.delete(directory);
}
function invocation(command, args, options = {}) {
  assert.ok(Object.hasOwn(tools, command), "No fallback command admission");
  assert.ok(
    Array.isArray(args) && args.every((value) => typeof value === "string"),
  );
  assert.ok(args.join("").length <= 64 * 1024);
  if (command === "jq") {
    assert.equal(args[0], "-e");
    assert.ok(
      [summaryFilter, ownershipFilter, exitFilter].includes(args.at(-1)),
    );
  } else if (command === "awk") {
    assert.equal(args.length, 1);
    assert.equal(args[0], countFilter);
  } else {
    assert.equal(args[0], "-c");
    assert.equal(args[2], "fixture");
    if (![boundedScript, publicScript].includes(args[1])) {
      assert.ok(
        [emergencyScript(""), emergencyScript("cp() { return 1; }")].includes(
          args[1],
        ),
      );
      admitted(args[3]);
      assert.match(args[4], /^[a-f0-9]{32}$/);
    }
  }
  const input = options.input ?? "";
  assert.ok(
    typeof input === "string" && Buffer.byteLength(input) <= 8 * 1024 * 1024,
  );
  const result = realSpawn(tools[command], args, {
    input,
    encoding: "utf8",
    timeout: 2000,
    maxBuffer: 65536,
    env: environment,
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null);
  return result;
}
function synthetic(context, values) {
  return new vm.SyntheticModule(
    Object.keys(values),
    function initialize() {
      for (const [name, value] of Object.entries(values))
        this.setExport(name, value);
    },
    { context },
  );
}
async function originalTests() {
  const callbacks = [];
  const context = vm.createContext({
    URL,
    process: { env: { PATH: environment.PATH } },
  });
  const realmArray = vm.runInContext("values => Array.from(values)", context);
  const safeFs = {
    mkdtempSync(prefix) {
      assert.equal(
        prefix,
        path.join(os.tmpdir(), "simplestchat-emergency-evidence."),
      );
      return temporary();
    },
    readFileSync(value, ...args) {
      const name = value instanceof URL ? fileURLToPath(value) : String(value);
      if (
        !references
          .slice(0, 2)
          .some(([entry]) => path.join(root, entry) === name)
      )
        admitted(name);
      return fs.readFileSync(name, ...args);
    },
    readdirSync(value, ...args) {
      return realmArray(fs.readdirSync(admitted(value), ...args));
    },
    rmSync(value, options) {
      assert.equal(options.recursive, true);
      assert.equal(options.force, true);
      assert.equal(Object.keys(options).length, 2);
      remove(String(value));
    },
    statSync(value, ...args) {
      return fs.statSync(admitted(value), ...args);
    },
    writeFileSync(value, ...args) {
      return fs.writeFileSync(admitted(value), ...args);
    },
  };
  const modules = {
    "node:assert/strict": { default: assert },
    "node:child_process": { spawnSync: invocation },
    "node:fs": safeFs,
    "node:os": { tmpdir: os.tmpdir },
    "node:path": { default: path },
    "node:test": {
      default: (name, callback) => callbacks.push({ name, callback }),
    },
  };
  const module = new vm.SourceTextModule(testsSource, {
    context,
    identifier: path.join(root, references[2][0]),
    initializeImportMeta(meta) {
      meta.url = new URL(`file://${path.join(root, references[2][0])}`).href;
    },
  });
  await module.link((name) => {
    assert.ok(
      Object.hasOwn(modules, name),
      "No new reference dependency fallback",
    );
    return synthetic(context, modules[name]);
  });
  await module.evaluate();
  assert.equal(callbacks.length, 47);
  for (const { callback } of callbacks) {
    const after = [];
    try {
      await callback({ after: (fn) => after.push(fn) });
    } finally {
      for (const fn of after.reverse()) await fn();
    }
  }
  assert.equal(owned.size, 0);
  return callbacks.length;
}
const defaultSelection = {
  clients: 10,
  rooms: 1,
  duration: 60,
  rampUp: 5,
  warmup: 10,
  run: "a".repeat(32),
  server: `sha256:${"b".repeat(64)}`,
  generator: `sha256:${"c".repeat(64)}`,
};
function jqArguments(selection) {
  assert.deepEqual(
    Object.keys(selection).sort(),
    Object.keys(defaultSelection).sort(),
  );
  return Object.entries(defaultSelection).flatMap(([key, defaultValue]) => {
    const value = selection[key];
    if (typeof defaultValue === "number")
      assert.ok(Number.isInteger(value) && value >= 0 && value <= 300);
    else assert.ok(typeof value === "string" && value.length <= 128);
    return [
      typeof value === "number" ? "--argjson" : "--arg",
      key,
      String(value),
    ];
  });
}
function runCase(options) {
  assert.ok(options && typeof options === "object" && !Array.isArray(options));
  let result;
  switch (options.kind) {
    case "summary":
      result = invocation(
        "jq",
        [
          "-e",
          ...jqArguments(options.selection ?? defaultSelection),
          summaryFilter,
        ],
        { input: options.input },
      );
      break;
    case "counts":
      result = invocation("awk", [countFilter], { input: options.input });
      break;
    case "ownership": {
      const expected = options.expected ?? {
        id: "ID",
        image: "IMAGE",
        run: "RUN",
        role: "server",
      };
      assert.deepEqual(Object.keys(expected).sort(), [
        "id",
        "image",
        "role",
        "run",
      ]);
      const args = Object.entries(expected).flatMap(([key, value]) => {
        assert.ok(typeof value === "string" && value.length <= 128);
        return ["--arg", key, value];
      });
      result = invocation("jq", ["-e", ...args, ownershipFilter], {
        input: options.input,
      });
      break;
    }
    case "exit":
      result = invocation("jq", ["-e", exitFilter], { input: options.input });
      break;
    case "bounded":
      assert.ok(
        typeof options.value === "string" && options.value.length <= 128,
      );
      result = invocation("bash", [
        "-c",
        boundedScript,
        "fixture",
        options.value,
        "1",
        "4",
      ]);
      assert.equal(result.stdout, "");
      break;
    case "public":
      assert.ok(
        typeof options.containers === "string" &&
          options.containers.length <= 256,
      );
      assert.ok(
        Number.isInteger(options.status) &&
          options.status >= 0 &&
          options.status <= 255,
      );
      result = invocation("bash", [
        "-c",
        publicScript,
        "fixture",
        options.containers,
        String(options.status),
      ]);
      assert.equal(result.stdout, "", "No identity in guard output");
      break;
    case "emergency": {
      assert.ok(options.prior === null || typeof options.prior === "string");
      assert.ok(
        options.prior === null || Buffer.byteLength(options.prior) <= 16384,
      );
      const directory = temporary();
      try {
        const filename = path.join(directory, "result.json");
        if (options.prior !== null)
          fs.writeFileSync(filename, options.prior, { mode: 0o600 });
        const script = emergencyScript(
          options.copyFailure ? "cp() { return 1; }" : "",
        );
        result = invocation("bash", [
          "-c",
          script,
          "fixture",
          directory,
          defaultSelection.run,
        ]);
        const bytes = fs.readFileSync(filename, "utf8");
        if (options.copyFailure && options.prior !== null) {
          assert.equal(bytes, options.prior);
          assert.equal(result.stdout, "false");
          return {
            status: result.status,
            passed: false,
            retained: false,
            originalUnchanged: true,
          };
        }
        assert.equal(result.stdout, "true");
        const record = JSON.parse(bytes);
        assert.equal(record.completed, false);
        assert.equal(record.passed, false);
        assert.equal(record.error, "supervisor_or_launcher_interrupted");
        assert.equal(record.runId, defaultSelection.run);
        if (options.prior === null) assert.equal(record.retainedResult, null);
        else {
          assert.match(
            record.retainedResult,
            /^result-before-emergency\.[A-Za-z0-9]+$/,
          );
          const retained = path.join(directory, record.retainedResult);
          assert.equal(fs.readFileSync(retained, "utf8"), options.prior);
          assert.equal(fs.statSync(retained).mode & 0o7777, 0o600);
        }
        return {
          status: result.status,
          passed: true,
          retained: options.prior !== null,
          originalUnchanged: options.prior === null,
        };
      } finally {
        remove(directory);
      }
    }
    default:
      throw new Error("Unsupported inert reference case");
  }
  return { status: result.status, passed: result.status === 0 };
}
function readInput() {
  const chunks = [];
  let count = 0;
  while (count <= 8 * 1024 * 1024) {
    const buffer = Buffer.alloc(Math.min(65536, 8 * 1024 * 1024 + 1 - count));
    const bytes = fs.readSync(0, buffer, 0, buffer.length, null);
    if (!bytes) break;
    count += bytes;
    chunks.push(buffer.subarray(0, bytes));
  }
  assert.ok(count <= 8 * 1024 * 1024);
  return JSON.parse(Buffer.concat(chunks, count).toString("utf8"));
}
function provider(name) {
  // One True AWK/Gawk use --version; Mawk exposes its version with -W version.
  // Record the actual provider, without claiming all providers share coercion.
  for (const args of name === "awk"
    ? [["--version"], ["-W", "version"]]
    : [["--version"]]) {
    const result = realSpawn(tools[name], args, {
      encoding: "utf8",
      timeout: 2000,
      maxBuffer: 4096,
      env: environment,
    });
    assert.ifError(result.error);
    if (result.status !== 0) continue;
    const first = (result.stdout || result.stderr).split("\n")[0];
    assert.ok(first.length <= 200 && /^[\x20-\x7e]+$/.test(first));
    return first;
  }
  throw new Error("Unsupported inert provider metadata.");
}
try {
  const request = readInput();
  assert.ok(Array.isArray(request.cases) && request.cases.length <= 512);
  const originalCount = await originalTests();
  const results = request.cases.map(runCase);
  const output = JSON.stringify({
    originalTests: originalCount,
    provider: Object.fromEntries(
      ["jq", "awk", "bash"].map((name) => [name, provider(name)]),
    ),
    results,
  });
  assert.ok(Buffer.byteLength(output) <= 256 * 1024);
  fs.writeSync(1, output);
} catch {
  process.stderr.write("Inert container benchmark reference failed.\n");
  process.exitCode = 1;
} finally {
  for (const directory of owned) remove(directory);
}
