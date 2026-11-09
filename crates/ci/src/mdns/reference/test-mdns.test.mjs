import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { interfaceFacts, nativeProbe, parseNativeProbe, runCli, summarizeLogs, systemSnapshot } from '../test-mdns.mjs';

const success = (stdout = '') => ({ stdout, exitCode: 0, timedOut: false });
const interfaces = { loopback: [{ family: 'IPv4', address: '127.0.0.1', internal: true }], assigned: [{ family: 'IPv4', address: '192.0.2.1', internal: false }] };
const native = { stage: 'callback', error: 0, callbackObserved: true, timedOut: false, deallocated: true };

test('mDNS preflight accepts only an IPv4 assigned to a non-loopback local interface', () => {
  assert.deepEqual(interfaceFacts('192.0.2.1', interfaces), { ipv4Addresses: 2, nonLoopbackIpv4Addresses: 1, selectedAddressAssigned: true });
  for (const address of [undefined, '', '::1', 'example.test', '127.0.0.1', '192.0.2.2']) {
    assert.throws(() => interfaceFacts(address, interfaces), /assigned/);
  }
});

test('scoped unified logs retain numeric counts and discard private strings and unrelated messages', () => {
  const output = [
    { eventMessage: 'private-host.local NetworkMDNSRegister callback - registerMDNSNameCallback with error 0' },
    { eventMessage: '192.0.2.100 NetworkMDNSRegister callback - registerMDNSNameCallback with error -65570' },
    { eventMessage: 'NetworkMDNSRegister::registerMDNSName DNSServiceCreateConnection error -65563' },
    { eventMessage: 'NetworkMDNSRegister::registerMDNSName DNSServiceRegisterRecord error -65540' },
    { eventMessage: 'Sandbox: WebKit deny network mDNS private-host' },
    { eventMessage: 'unrelated private-host event' },
  ].map(value => JSON.stringify(value)).join('\n');
  const result = summarizeLogs(`${output}\nnot-json\n${JSON.stringify({ eventMessage: null })}`);
  assert.deepEqual(result, { registrationCallbacks: 2, callbackErrors: { 0: 1, '-65570': 1 }, createConnectionErrors: { '-65563': 1 }, registerRecordErrors: { '-65540': 1 }, sandboxDenials: 1 });
  assert.doesNotMatch(JSON.stringify(result), /private-host|192\.0\.2|unrelated/);
});

test('OS snapshot reads the system service label then state and narrowly scoped logs without retaining raw output', async () => {
  const calls = [];
  const result = await systemSnapshot(async (binary, args) => {
    calls.push([binary, args]);
    if (binary.endsWith('plutil')) return success('com.apple.mDNSResponder.reloaded\n');
    if (binary.endsWith('launchctl')) return success('private-path\n\tstate = running\n');
    return success(`${JSON.stringify({ eventMessage: 'private-host registerMDNSNameCallback with error 0' })}\n`);
  });
  assert.equal(calls[0][0], '/usr/bin/plutil');
  assert.deepEqual(calls[1], ['/bin/launchctl', ['print', 'system/com.apple.mDNSResponder.reloaded']]);
  assert.equal(calls[2][0], '/usr/bin/log');
  assert.deepEqual(calls[2][1].slice(0, 8), ['show', '--last', '2m', '--style', 'ndjson', '--info', '--debug', '--predicate']);
  assert.match(calls[2][1][8], /NetworkMDNSRegister/);
  assert.match(calls[2][1][8], /process == "sandboxd"/);
  assert.equal(result.responder.state, 'running');
  assert.equal(result.logs.registrationCallbacks, 1);
  assert.doesNotMatch(JSON.stringify(result), /private-path|private-host/);
});

test('unknown service labels are never interpolated into launchctl commands', async () => {
  const calls = [];
  const result = await systemSnapshot(async (binary, args) => {
    calls.push([binary, args]);
    return success('untrusted service\n');
  });
  assert.equal(result.responder.readable, false);
  assert.equal(result.responder.state, 'unavailable');
  assert.equal(calls.some(([binary]) => binary.endsWith('launchctl')), false);
});

test('native success requires a successful callback, explicit deallocation and zero child status', () => {
  const parsed = parseNativeProbe(success(JSON.stringify({ ...native, privateHost: 'never-save.local' })));
  assert.equal(parsed.passed, true);
  assert.doesNotMatch(JSON.stringify(parsed), /never-save|privateHost/);
  for (const patch of [{ stage: 'register_record' }, { error: -65570 }, { callbackObserved: false }, { timedOut: true }, { deallocated: false }]) {
    assert.equal(parseNativeProbe(success(JSON.stringify({ ...native, ...patch }))).passed, false);
  }
  assert.equal(parseNativeProbe({ ...success(JSON.stringify(native)), exitCode: 1 }).passed, false);
  assert.equal(parseNativeProbe({ ...success(JSON.stringify(native)), timedOut: true }).passed, false);
  for (const stdout of ['not-json', 'null', JSON.stringify({ ...native, stage: 'private-stage' }), JSON.stringify({ ...native, error: '0' })]) {
    assert.equal(parseNativeProbe(success(stdout)).passed, false);
  }
});

test('compiled probe and temporary directory are owned, bounded and removed after a child failure', async () => {
  let directory;
  const calls = [];
  const result = await nativeProbe('192.0.2.1', async (binary, args, timeout) => {
    calls.push({ binary, args, timeout });
    if (binary === '/usr/bin/cc') {
      directory = join(args.at(-1), '..');
      return success();
    }
    assert.equal(args[0], '192.0.2.1');
    assert.match(args[1], /^[a-f\d-]{36}$/);
    return { stdout: JSON.stringify({ ...native, stage: 'callback_timeout', callbackObserved: false, timedOut: true }), exitCode: 1, timedOut: false };
  });
  assert.equal(calls[0].timeout, 15000);
  assert.equal(calls[1].timeout, 8000);
  assert.equal(result.passed, false);
  await assert.rejects(stat(directory), { code: 'ENOENT' });
});

test('CLI refuses missing opt-in, unsupported platforms, invalid modes or artifacts before invoking diagnostics', async () => {
  for (const options of [
    { env: {} }, { platform: 'linux' }, { args: [] }, { args: ['unknown'] }, { env: { MDNS_DIAGNOSTIC_E2E: '1' } },
  ]) {
    let invoked = false;
    const status = await runCli({ args: ['preflight'], env: { MDNS_DIAGNOSTIC_E2E: '1', E2E_ARTIFACTS: '/unused' }, platform: 'darwin', snapshot: () => { invoked = true; }, stderr: { write() {} }, ...options });
    assert.equal(status, 2);
    assert.equal(invoked, false);
  }
});

test('CLI records system evidence before preflight and preserves a failed registration outcome', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'mdns-cli-test-'));
  const order = [];
  try {
    const status = await runCli({
      args: ['preflight'], platform: 'darwin', interfaces: () => interfaces,
      env: { MDNS_DIAGNOSTIC_E2E: '1', E2E_ARTIFACTS: directory, TEST_ANNOUNCE_IP: '192.0.2.1' },
      snapshot: async () => { order.push('system'); return { responder: { readable: true }, logs: { readable: true } }; },
      probe: async () => { order.push('native'); return { passed: false, stage: 'callback_timeout' }; },
      stdout: { write() {} },
    });
    assert.deepEqual(order, ['system', 'native']);
    assert.equal(status, 1);
    const report = JSON.parse(await readFile(join(directory, 'mdns-preflight-results.json'), 'utf8'));
    assert.equal(report.passed, false);
    assert.equal(report.native.stage, 'callback_timeout');
    assert.doesNotMatch(JSON.stringify(report), /192\.0\.2\.1/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test('diagnostics refuse to overwrite earlier evidence or follow an artifact symlink', async () => {
  const directory = await mkdtemp(join(tmpdir(), 'mdns-evidence-test-'));
  try {
    const original = join(directory, 'original.json');
    await writeFile(original, 'prior-evidence');
    await symlink(original, join(directory, 'mdns-snapshot-results.json'));
    const status = await runCli({
      args: ['snapshot'], platform: 'darwin', interfaces: () => interfaces,
      env: { MDNS_DIAGNOSTIC_E2E: '1', E2E_ARTIFACTS: directory, TEST_ANNOUNCE_IP: '192.0.2.1' },
      snapshot: async () => ({ responder: { readable: true }, logs: { readable: true } }),
      stdout: { write() { assert.fail('failed evidence writes are not printed as success'); } },
      stderr: { write() {} },
    });
    assert.equal(status, 1);
    assert.equal(await readFile(original, 'utf8'), 'prior-evidence');
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});
