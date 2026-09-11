import assert from 'node:assert/strict';
import test from 'node:test';
import { detectTestMediaIp, runCli, selectTestMediaIp } from '../test-media-ip.mjs';

const ipv4 = (address, extra = {}) => ({ address, family: 'IPv4', internal: false, ...extra });
const interfaces = {
  lo: [ipv4('127.0.0.1', { internal: true })],
  docker0: [ipv4('172.17.0.1')],
  tun0: [ipv4('10.20.0.1')],
  eth0: [{ address: 'fe80::1234', family: 'IPv6', internal: false }, ipv4('192.0.2.20'), ipv4('192.0.2.21')],
  en0: [ipv4('198.51.100.30')],
};
const linuxRoute = (dev = 'eth0', extra = {}) => ({ dst: 'default', dev, ...extra });
const macRoute = '   route to: default\ndestination: default\n    gateway: 198.51.100.1\n  interface: en0\n      flags: <UP,GATEWAY,DONE,STATIC>\n';

test('Linux selects the first owned IPv4 on the default-route interface, not Docker or VPN entries', () => {
  assert.equal(selectTestMediaIp('linux', JSON.stringify([linuxRoute()]), interfaces), '192.0.2.20');
});

test('macOS selects the interface reported by route', () => {
  assert.equal(selectTestMediaIp('darwin', macRoute, interfaces), '198.51.100.30');
});

test('multiple Linux defaults prefer the lowest metric regardless of listing order', () => {
  const routes = [linuxRoute('en0', { metric: 600 }), linuxRoute('eth0', { metric: 100 })];
  for (const order of [routes, [...routes].reverse()]) {
    assert.equal(selectTestMediaIp('linux', JSON.stringify(order), interfaces), '192.0.2.20');
  }
  assert.equal(selectTestMediaIp('linux', JSON.stringify([linuxRoute('en0', { metric: 1 }), linuxRoute()]), interfaces), '192.0.2.20');
});

test('equal-priority routes must agree on the interface', () => {
  assert.throws(() => selectTestMediaIp('linux', JSON.stringify([linuxRoute(), linuxRoute('en0')]), interfaces), /Ambiguous/);
  assert.equal(selectTestMediaIp('linux', JSON.stringify([linuxRoute(), linuxRoute()]), interfaces), '192.0.2.20');
});

test('selection accepts numeric IPv4 families and ignores unusable or non-IPv4 addresses', () => {
  const unusable = ['0.0.0.0', '0.1.2.3', '127.0.0.1', '127.2.3.4', '169.254.1.1',
    '224.0.0.1', '255.255.255.255', 'localhost', '192.0.2.01', '192.0.2.20 '];
  const entries = [null, ipv4(undefined), ipv4(12345), ipv4('192.0.2.99', { internal: true }), ...unusable.map(address => ipv4(address)),
    { address: '::1', family: 'IPv6', internal: false }, ipv4('192.0.2.44', { family: 4 })];
  assert.equal(selectTestMediaIp('linux', JSON.stringify([linuxRoute()]), { eth0: entries }), '192.0.2.44');
});

test('an unusable preferred interface never falls back to another route or arbitrary interface', () => {
  const route = JSON.stringify([linuxRoute('missing', { metric: 0 }), linuxRoute('eth0', { metric: 20 })]);
  assert.throws(() => selectTestMediaIp('linux', route, interfaces), /missing has no usable owned/);
  for (const entries of [undefined, null, [], [ipv4('127.0.0.1')], [ipv4('192.0.2.20', { internal: true })]]) {
    assert.throws(() => selectTestMediaIp('linux', JSON.stringify([linuxRoute()]), { ...interfaces, eth0: entries }), /no usable owned/);
  }
});

test('empty, malformed, non-default, and unsupported Linux routes fail clearly', () => {
  for (const output of ['', ' ', undefined, 'not JSON', '{}', '[]', '[null]', '[[]]',
    JSON.stringify([{ ...linuxRoute(), dst: '192.0.2.0/24' }]),
    JSON.stringify([linuxRoute('')]), JSON.stringify([linuxRoute('eth0 extra')]),
    JSON.stringify([linuxRoute('eth0', { metric: -1 })]),
    JSON.stringify([linuxRoute('eth0', { metric: '100' })]),
    JSON.stringify([linuxRoute('eth0', { metric: 1.5 })]),
    JSON.stringify([linuxRoute('eth0', { type: 'blackhole' })]),
    JSON.stringify([{ dst: 'default', nexthops: [{ dev: 'eth0' }, { dev: 'en0' }] }])]) {
    assert.throws(() => selectTestMediaIp('linux', output, interfaces), /route|JSON/i, String(output));
  }
});

test('missing or ambiguous macOS interface output fails rather than assuming en0', () => {
  for (const output of ['', 'route: not found', 'interface:', 'interface:\nen0', 'interface: en0 extra', `${macRoute}\ninterface: eth0\n`]) {
    assert.throws(() => selectTestMediaIp('darwin', output, interfaces), /route|interface/i);
  }
});

test('discovery invokes only the platform-specific read-only route command with a deadline', () => {
  for (const [platform, command, args, output, expected] of [
    ['linux', 'ip', ['-j', '-4', 'route', 'show', 'default'], JSON.stringify([linuxRoute()]), '192.0.2.20'],
    ['darwin', 'route', ['-n', 'get', 'default'], macRoute, '198.51.100.30'],
  ]) {
    let calls = 0;
    const result = detectTestMediaIp({ platform, networkInterfaces: () => interfaces,
      execFileSync(actualCommand, actualArgs, options) {
        calls++;
        assert.equal(actualCommand, command);
        assert.deepEqual(actualArgs, args);
        assert.equal(options.encoding, 'utf8');
        assert.equal(options.timeout, 3000);
        assert.equal(options.maxBuffer, 65536);
        assert.deepEqual(options.stdio, ['ignore', 'pipe', 'pipe']);
        assert.equal(options.env.LC_ALL, 'C');
        return output;
      },
    });
    assert.equal(result, expected);
    assert.equal(calls, 1);
  }
});

test('unsupported platforms fail before calling any discovery dependencies', () => {
  const unexpected = () => assert.fail('Discovery should not run');
  assert.throws(() => detectTestMediaIp({ platform: 'win32', networkInterfaces: unexpected, execFileSync: unexpected }), /Unsupported platform/);
});

test('command failure, missing tools, and timeouts fail without printing command output', () => {
  for (const code of ['ENOENT', 'ETIMEDOUT', undefined]) {
    assert.throws(() => detectTestMediaIp({ platform: 'linux', networkInterfaces: () => assert.fail('Do not read interfaces after failure'),
      execFileSync() { throw Object.assign(new Error('private command output'), { code }); },
    }), error => {
      assert.match(error.message, /Cannot read default route with ip/);
      assert.doesNotMatch(error.message, /private command output/);
      return true;
    });
  }
});

test('CLI success prints only the selected IP, while errors print only to stderr', () => {
  let output = '', errors = '';
  const io = { stdout: { write: value => { output += value; } }, stderr: { write: value => { errors += value; } } };
  assert.equal(runCli({ ...io, detect: () => '192.0.2.20' }), 0);
  assert.equal(output, '192.0.2.20\n');
  assert.equal(errors, '');
  output = '';
  assert.equal(runCli({ ...io, detect: () => { throw new Error('No IPv4 default route found'); } }), 1);
  assert.equal(output, '');
  assert.match(errors, /^Cannot select test media IP: No IPv4 default route found\n$/);
});

test('CLI arguments fail before discovery without changing the environment', () => {
  const before = { ...process.env };
  let errors = '';
  assert.equal(runCli({ args: ['unexpected'], detect: () => assert.fail('Do not discover on bad arguments'),
    stdout: { write: () => assert.fail('No stdout on failure') }, stderr: { write: value => { errors += value; } },
  }), 1);
  assert.match(errors, /Usage:/);
  assert.deepEqual({ ...process.env }, before);
});
