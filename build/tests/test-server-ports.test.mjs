import assert from 'node:assert/strict';
import { EventEmitter, once } from 'node:events';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import test from 'node:test';
import { runInNewContext } from 'node:vm';

const source = await readFile(new URL('./test-server.test.mjs', import.meta.url), 'utf8');
const start = source.indexOf('async function unusedPort(');
const end = source.indexOf('\nasync function run(', start);
assert.ok(start >= 0 && end > start);

function fixture(candidates, { udpError } = {}) {
  const active = new Set();
  const calls = [];
  class Socket extends EventEmitter {
    constructor(protocol) { super(); this.protocol = protocol; }
    listen() {
      this.port = candidates.shift();
      assert.ok(this.port, 'unexpected port allocation');
      active.add(`tcp:${this.port}`);
      calls.push(['listen', this.port]);
      queueMicrotask(() => this.emit('listening'));
      return this;
    }
    bind(port) {
      this.port = port;
      assert.ok(active.has(`tcp:${port}`), 'TCP lease must remain held during the UDP probe');
      calls.push(['bind', port]);
      const error = udpError?.(port);
      if (error) queueMicrotask(() => this.emit('error', error));
      else {
        active.add(`udp:${port}`);
        queueMicrotask(() => this.emit('listening'));
      }
      return this;
    }
    address() { return { port: this.port }; }
    close(callback) {
      calls.push(['close', this.protocol, this.port]);
      active.delete(`${this.protocol}:${this.port}`);
      queueMicrotask(callback);
    }
  }
  const allocate = runInNewContext(`${source.slice(start, end)}; unusedPort`, {
    net: { createServer: () => new Socket('tcp') },
    dgram: { createSocket: () => new Socket('udp') },
    once,
  });
  return { allocate, active, calls };
}

test('automatic HTTP allocation cannot reuse a released TURN or HTTPS port', async () => {
  const f = fixture([49315, 49316, 49317]);
  assert.equal(await f.allocate('tcp', ['49315', '49316']), 49317);
  assert.equal(f.active.size, 0);
  assert.deepEqual(f.calls, [
    ['listen', 49315], ['close', 'tcp', 49315],
    ['listen', 49316], ['close', 'tcp', 49316],
    ['listen', 49317], ['close', 'tcp', 49317],
  ]);
});

test('TURN probes both protocols while holding its own TCP lease and retires a colliding candidate', async () => {
  const occupied = Object.assign(new Error('occupied UDP candidate'), { code: 'EADDRINUSE' });
  const f = fixture([49315, 49316], { udpError: port => port === 49315 ? occupied : null });
  assert.equal(await f.allocate('both'), 49316);
  assert.equal(f.active.size, 0);
  assert.deepEqual(f.calls, [
    ['listen', 49315], ['bind', 49315], ['close', 'udp', 49315], ['close', 'tcp', 49315],
    ['listen', 49316], ['bind', 49316], ['close', 'udp', 49316], ['close', 'tcp', 49316],
  ]);
});

test('fixture candidate retries are bounded and preserve unexpected binding failures', async () => {
  const f = fixture(Array(8).fill(49315));
  await assert.rejects(f.allocate('tcp', [49315]), /after 8 attempts/);
  assert.equal(f.active.size, 0);
  assert.equal(f.calls.filter(call => call[0] === 'listen').length, 8);
  const refused = Object.assign(new Error('fixture permission failure'), { code: 'EPERM' });
  const unexpected = fixture([49316, 49317], { udpError: () => refused });
  await assert.rejects(unexpected.allocate('both'), error => error === refused);
  assert.equal(unexpected.active.size, 0);
  assert.equal(unexpected.calls.filter(call => call[0] === 'listen').length, 1);
});

test('server fixture excludes relay ports but preserves explicit occupied-port overrides without child retries', async () => {
  const from = source.indexOf('async function run(');
  const to = source.indexOf('\nasync function shutdownReport(', from);
  const allocations = [], children = [];
  const run = runInNewContext(`${source.slice(from, to)}; run`, {
    assert,
    path,
    repoRoot: '/owned-fixture',
    xtask: '/owned-fixture/xtask',
    fixture: '/owned-fixture/server',
    os: { tmpdir: () => '/owned-fixture' },
    process: { execPath: '/owned-node', env: { PATH: '/owned-bin' } },
    mkdtemp: async () => '/owned-fixture/artifacts',
    rm: async () => {},
    unusedPort: async (protocol, excluded) => {
      allocations.push([protocol, [...excluded]]);
      return protocol === 'tcp' ? 49317 : 49318;
    },
    spawn(_command, _arguments, options) {
      children.push(options.env);
      const child = new EventEmitter();
      child.stdout = new EventEmitter();
      child.stderr = new EventEmitter();
      queueMicrotask(() => child.emit('close', 23, null));
      return child;
    },
    once,
    setTimeout,
    clearTimeout,
  });
  const t = { after() {} };
  const environment = { TURN_E2E: '1', TEST_TURN_PORT: '49315', TEST_HTTPS_PORT: '49316' };
  assert.equal((await run(t, environment)).status, 23);
  assert.deepEqual(allocations, [['tcp', ['49315', '49316']], ['udp', ['49315']]]);
  assert.equal(children[0].TEST_SERVER_PORT, '49317');
  assert.equal(children[0].TEST_MEDIA_PORT, '49318');
  allocations.length = 0;
  assert.equal((await run(t, { ...environment, TEST_SERVER_PORT: '49315', TEST_MEDIA_PORT: '49315' })).status, 23);
  assert.deepEqual(allocations, []);
  assert.equal(children.length, 2, 'each operation starts its child exactly once');
  assert.equal(children[1].TEST_SERVER_PORT, '49315');
  assert.equal(children[1].TEST_MEDIA_PORT, '49315');
});
