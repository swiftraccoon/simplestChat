// Execute the unchanged mDNS JavaScript with inert command and OS adapters.
// The reference tests own only temporary files; no system diagnostic is run.
import assert from 'node:assert/strict';
import { readFile, mkdtemp, writeFile, rm } from 'node:fs/promises';
import * as promises from 'node:fs/promises';
import { tmpdir } from 'node:os';
import * as path from 'node:path';
import * as url from 'node:url';
import { isIPv4 } from 'node:net';
import { promisify } from 'node:util';
import { SourceTextModule, SyntheticModule, createContext } from 'node:vm';

const sourceUrl = new URL('./reference/test-mdns.mjs', import.meta.url);
const testsUrl = new URL('./reference/test-mdns.test.mjs', import.meta.url);
const fixedUuid = '11111111-1111-4111-8111-111111111111';
const fixedTime = '2026-10-22T00:00:00.000Z';
const denied = () => {
  throw new Error('oracle_system_operation_refused');
};
const tests = [];
const context = createContext({
  URL,
  Date: class extends Date {
    constructor(...args) {
      super(...(args.length ? args : [fixedTime]));
    }
    static now() {
      return Date.parse(fixedTime);
    }
  },
  process: {
    argv: ['node', '/oracle-not-the-reference'],
    env: {},
    platform: 'oracle',
    stdout: { write: denied },
    stderr: { write: denied },
  },
});
const modules = new Map();
function synthetic(name, exports) {
  const module = new SyntheticModule(
    Object.keys(exports),
    function () {
      for (const [key, value] of Object.entries(exports)) this.setExport(key, value);
    },
    { context, identifier: name },
  );
  modules.set(name, module);
}
synthetic('node:assert/strict', { default: assert });
synthetic('node:child_process', { execFile: denied });
synthetic('node:crypto', { randomUUID: () => fixedUuid });
synthetic('node:fs/promises', promises);
synthetic('node:net', { isIPv4 });
synthetic('node:os', { tmpdir, networkInterfaces: denied });
synthetic('node:path', path);
synthetic('node:url', url);
synthetic('node:util', { promisify });
synthetic('node:test', { default: (name, run) => tests.push({ name, run }) });
const source = new SourceTextModule(await readFile(sourceUrl, 'utf8'), {
  context,
  identifier: sourceUrl.href,
  initializeImportMeta(meta) {
    meta.url = sourceUrl.href;
  },
});
async function link(specifier) {
  if (specifier === '../test-mdns.mjs') return source;
  const module = modules.get(specifier);
  if (!module) throw new Error('oracle_import_refused');
  return module;
}
await source.link(link);
await source.evaluate();

async function selfTest() {
  const suite = new SourceTextModule(await readFile(testsUrl, 'utf8'), {
    context,
    identifier: testsUrl.href,
  });
  await suite.link(link);
  await suite.evaluate();
  assert.equal(tests.length, 9);
  for (const { run } of tests) {
    const finalizers = [];
    try {
      await run({ after: (callback) => finalizers.push(callback) });
    } finally {
      for (const callback of finalizers.reverse()) await callback();
    }
  }
  return { unchangedReferenceTests: tests.length };
}

const success = (stdout = '') => ({ stdout, exitCode: 0, timedOut: false });
const defaultProbe = {
  stage: 'callback',
  error: 0,
  callbackObserved: true,
  timedOut: false,
  deallocated: true,
};

function interfaces(rows) {
  const result = Object.create(null);
  for (const row of rows ?? [
    { name: 'lo0', loopback: true, addresses: ['127.0.0.1'] },
    { name: 'en0', loopback: false, addresses: ['192.0.2.1'] },
  ]) {
    // Node's Darwin libuv adapter filters these before networkInterfaces().
    const flags = row.flags ?? ['UP', 'RUNNING'];
    if (
      row.up === false ||
      row.running === false ||
      !flags.includes('UP') ||
      !flags.includes('RUNNING')
    )
      continue;
    const values = (result[row.name] ??= []);
    for (const address of row.addresses ?? []) {
      values.push({
        family: isIPv4(address) ? 'IPv4' : 'IPv6',
        address,
        internal: row.loopback === true || flags.includes('LOOPBACK'),
      });
    }
  }
  return result;
}

async function runCase(options) {
  assert.ok(options && typeof options === 'object' && !Array.isArray(options));
  const known = new Set([
    'mode',
    'args',
    'address',
    'interfaceRows',
    'plistLabel',
    'plistExit',
    'plistTimedOut',
    'launchState',
    'launchExit',
    'launchTimedOut',
    'logs',
    'logsExit',
    'logsTimedOut',
    'compileExit',
    'compileTimedOut',
    'probe',
    'probeStdout',
    'probeExit',
    'probeTimedOut',
    'throwStage',
    'artifactExists',
    'optIn',
    'noArtifacts',
    'platform',
  ]);
  assert.ok(Object.keys(options).every((key) => known.has(key)));
  const mode = options.mode ?? 'preflight';
  if (options.artifactExists) assert.ok(['preflight', 'snapshot'].includes(mode));
  const directory = await mkdtemp(path.join(tmpdir(), 'simplestchat-mdns-oracle-'));
  const commands = [];
  let printed = '';
  const command = async (binary, args, timeout = 5000) => {
    const roles = {
      '/usr/bin/plutil': 'plutil',
      '/bin/launchctl': 'launchctl',
      '/usr/bin/log': 'log',
      '/usr/bin/cc': 'cc',
    };
    const role = roles[binary] ?? 'probe';
    const normalized =
      role === 'probe'
        ? ['<assigned-ipv4>', '<uuid>']
        : role === 'cc'
          ? args.map((value) =>
              value.endsWith('test-mdns.c')
                ? '<probe-source>'
                : value.endsWith('/probe')
                  ? '<probe-binary>'
                  : value,
            )
          : args;
    commands.push({ role, args: normalized, timeoutMs: timeout });
    if (role === 'plutil')
      return {
        ...success(`${options.plistLabel ?? 'com.apple.mDNSResponder.reloaded'}\n`),
        exitCode: options.plistExit ?? 0,
        timedOut: options.plistTimedOut === true,
      };
    if (role === 'launchctl')
      return {
        ...success(`state = ${options.launchState ?? 'running'}\n`),
        exitCode: options.launchExit ?? 0,
        timedOut: options.launchTimedOut === true,
      };
    if (role === 'log')
      return {
        ...success(options.logs ?? ''),
        exitCode: options.logsExit ?? 0,
        timedOut: options.logsTimedOut === true,
      };
    if (role === 'cc')
      return {
        ...success(),
        exitCode: options.compileExit ?? 0,
        timedOut: options.compileTimedOut === true,
      };
    return {
      stdout: options.probeStdout ?? JSON.stringify(options.probe ?? defaultProbe),
      exitCode: options.probeExit ?? 0,
      timedOut: options.probeTimedOut === true,
    };
  };
  try {
    if (options.artifactExists)
      await writeFile(path.join(directory, `mdns-${mode}-results.json`), 'retained-prior-evidence');
    const status = await source.namespace.runCli({
      args: options.args ?? [mode],
      env: {
        MDNS_DIAGNOSTIC_E2E: options.optIn === false ? '' : '1',
        E2E_ARTIFACTS: options.noArtifacts ? '' : directory,
        TEST_ANNOUNCE_IP: Object.hasOwn(options, 'address') ? options.address : '192.0.2.1',
      },
      platform: options.platform ?? 'darwin',
      interfaces() {
        if (options.throwStage === 'interface') throw new Error('private-interface');
        return interfaces(options.interfaceRows);
      },
      async snapshot() {
        if (options.throwStage === 'snapshot') throw new Error('private-snapshot');
        return source.namespace.systemSnapshot(command);
      },
      async probe(address) {
        if (options.throwStage === 'probe') throw new Error('private-probe');
        return source.namespace.nativeProbe(address, command);
      },
      stdout: {
        write(value) {
          printed += value;
        },
      },
      stderr: { write() {} },
    });
    const report = printed ? JSON.parse(printed) : null;
    if (report) {
      assert.equal(report.startedAt, fixedTime);
      assert.equal(report.finishedAt, fixedTime);
      const saved = JSON.parse(
        await readFile(path.join(directory, `mdns-${mode}-results.json`), 'utf8'),
      );
      assert.deepEqual(saved, report);
      delete report.startedAt;
      delete report.finishedAt;
    }
    if (options.artifactExists)
      assert.equal(
        await readFile(path.join(directory, `mdns-${mode}-results.json`), 'utf8'),
        'retained-prior-evidence',
      );
    return { status, report, commands };
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

if (process.argv.slice(2).length === 1 && process.argv[2] === '--self-test') {
  process.stdout.write(`${JSON.stringify(await selfTest())}\n`);
} else {
  assert.equal(process.argv.length, 2);
  let input = '';
  for await (const chunk of process.stdin) {
    input += chunk;
    assert.ok(Buffer.byteLength(input) <= 256 * 1024);
  }
  const request = JSON.parse(input);
  assert.deepEqual(Object.keys(request), ['cases']);
  assert.ok(
    Array.isArray(request.cases) && request.cases.length > 0 && request.cases.length <= 128,
  );
  const results = [];
  for (const options of request.cases) results.push(await runCase(options));
  const output = JSON.stringify({ results });
  assert.ok(Buffer.byteLength(output) <= 1024 * 1024);
  process.stdout.write(`${output}\n`);
}
