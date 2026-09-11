import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { chmod, copyFile, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

const launcher = new URL('../run-local.sh', import.meta.url);
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

// Every command that could build, install, download, or start the application is
// intercepted. The launcher itself is copied into an isolated fake checkout.
const fakeTool = String.raw`
const fs = require('node:fs');
const path = require('node:path');
const [command, ...args] = process.argv.slice(2);
const env = process.env;
const record = name => fs.appendFileSync(env.FIXTURE_LOG, JSON.stringify({
  command: name, args, cwd: process.cwd(), env: Object.fromEntries([
    'RUSTC', 'RUSTDOC', 'OPENSSL_DIR', 'OPENSSL_STATIC', 'PKG_CONFIG_PATH',
    'PIP_CONSTRAINT', 'BIND_ADDR', 'PORT', 'ANNOUNCE_IP', 'MEDIA_WORKERS',
    'WEBRTC_SERVER_PORT_BASE', 'ALLOW_AD_HOC_ROOMS',
  ].map(key => [key, env[key]])),
}) + '\n');
record(command);
const fail = phase => {
  if (env.FIXTURE_FAIL === phase) process.exit(Number(env.FIXTURE_FAILURE_CODE || 37));
};
if (command === 'rustup') {
  if (args[0] === 'which') {
    const binary = args.find(arg => arg === 'rustc' || arg === 'rustdoc');
    fail('rustup-which');
    if (!binary || !args.includes(env.FIXTURE_CHANNEL)) process.exit(91);
    console.log(path.join(env.FIXTURE_ROOT, 'toolchain', 'bin', binary));
  } else if (args[0] === 'run') {
    if (args[1] !== env.FIXTURE_CHANNEL || args[2] !== 'cargo') process.exit(92);
    if (args.includes('--version')) console.log('cargo ' + env.FIXTURE_CHANNEL);
    else {
      fail('cargo');
      record('server');
      fail('server');
    }
  } else if (args[0] === '--version') console.log('rustup 1.29.0');
  else process.exit(93);
} else if (command === 'pinned-rustc' || command === 'pinned-rustdoc') {
  console.log((command === 'pinned-rustc' ? 'rustc ' : 'rustdoc ') + env.FIXTURE_CHANNEL);
} else if (['cargo', 'rustc', 'rustdoc'].includes(command)) {
  // Simulate an incompatible Homebrew compiler before any rustup proxy in PATH.
  console.error('Unpinned Homebrew tool was used: ' + command);
  process.exit(94);
} else if (command === 'npm') {
  if (args.includes('ci')) fail('npm-ci');
  else if (args.includes('build')) fail('npm-build');
  else if (args.includes('--version')) console.log('11.8.0');
  else process.exit(95);
} else if (command === 'install-openssl') {
  fail('openssl-install');
  const prefix = args[0];
  for (const file of ['lib/libssl.a', 'lib/libcrypto.a', 'include/openssl/ssl.h', 'lib/pkgconfig/openssl.pc']) {
    fs.mkdirSync(path.dirname(path.join(prefix, file)), { recursive: true });
    fs.writeFileSync(path.join(prefix, file), 'fixture only\n');
  }
} else if (command === 'uname') {
  console.log(env.FIXTURE_PLATFORM || 'Darwin');
} else if (command === 'route') {
  if (env.FIXTURE_ROUTE_FAIL === '1') process.exit(1);
  console.log('   route to: default\n  interface: en7');
} else if (command === 'ipconfig') {
  if (args.at(-1) === 'en7') console.log('192.0.2.7');
  else if (args.at(-1) === 'en0') console.log('192.0.2.20');
  else process.exit(1);
} else if (command === 'pkg-config') {
  if (args.includes('--modversion')) console.log('3.5.8');
  fail('pkg-config');
} else if (command === 'node') {
  // The inline availability preflight is covered separately. Never open ports
  // in these command-contract fixtures, and never execute real setup commands.
  if (args.includes('--version') || args.includes('-v')) console.log('v26.8.1');
}
else if (command === 'python3') console.log('Python 3.14.0');
else if (command === 'openssl') console.log('OpenSSL 3.5.8 fixture');
else if (['cc', 'c++', 'clang', 'clang++', 'cmake', 'make', 'xcrun'].includes(command)) {
  if (args.includes('--version')) console.log(command + ' fixture');
} else {
  console.error('Unexpected fake command: ' + command);
  process.exit(96);
}
`;

async function fixture(t, { channel = '1.98.1', installedOpenSsl = true, installedWeb = true, installedDist = true } = {}) {
  // macOS aliases /var to /private/var; compare canonical fixture paths with
  // the shell's physical cwd, without making the test platform-dependent.
  const temporary = await realpath(await mkdtemp(path.join(os.tmpdir(), 'simplestchat-launcher-test.')));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const root = path.join(temporary, 'checkout with spaces');
  const bin = path.join(temporary, 'fake-bin');
  const log = path.join(temporary, 'commands.jsonl');
  const dispatcher = path.join(temporary, 'fake-tool.cjs');
  await Promise.all([
    mkdir(path.join(root, 'build'), { recursive: true }),
    mkdir(path.join(root, 'toolchain/bin'), { recursive: true }),
    mkdir(path.join(root, 'web'), { recursive: true }),
    mkdir(bin),
    writeFile(dispatcher, fakeTool),
    writeFile(log, ''),
  ]);
  await copyFile(launcher, path.join(root, 'build/run-local.sh'));
  await writeFile(path.join(root, 'rust-toolchain.toml'), `[toolchain]\nchannel = "${channel}"\n`);
  await writeFile(path.join(root, 'build/pip-constraints.txt'), '# fixture only\n');
  await writeFile(path.join(root, 'Cargo.toml'), '[package]\nname = "simplestChat"\nversion = "0.0.0"\n');
  await writeFile(path.join(root, 'web/package.json'), '{"private":true}\n');
  await writeFile(path.join(root, 'web/package-lock.json'), '{"lockfileVersion":3}\n');
  if (installedWeb) await mkdir(path.join(root, 'web/node_modules'));
  if (installedDist) {
    await mkdir(path.join(root, 'web/dist'));
    await writeFile(path.join(root, 'web/dist/index.html'), '<!doctype html><title>Fixture only</title>\n');
  }

  const executable = async (file, command) => {
    await writeFile(file, `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(dispatcher)} ${quote(command)} "$@"\n`);
    await chmod(file, 0o755);
  };
  await Promise.all([
    ...['rustup', 'cargo', 'rustc', 'rustdoc', 'npm', 'node', 'uname', 'route', 'ipconfig',
      'pkg-config', 'python3', 'cc', 'c++', 'clang', 'clang++', 'cmake', 'make', 'xcrun', 'openssl']
      .map(command => executable(path.join(bin, command), command)),
    executable(path.join(root, 'build/install-openssl.sh'), 'install-openssl'),
    executable(path.join(root, 'toolchain/bin/rustc'), 'pinned-rustc'),
    executable(path.join(root, 'toolchain/bin/rustdoc'), 'pinned-rustdoc'),
  ]);
  const prefix = path.join(root, 'target/openssl-3.5.8');
  if (installedOpenSsl) {
    for (const file of ['lib/libssl.a', 'lib/libcrypto.a', 'include/openssl/ssl.h', 'lib/pkgconfig/openssl.pc']) {
      await mkdir(path.dirname(path.join(prefix, file)), { recursive: true });
      await writeFile(path.join(prefix, file), 'fixture only\n');
    }
    await mkdir(path.join(prefix, 'bin'));
    await executable(path.join(prefix, 'bin/openssl'), 'openssl');
  }

  return {
    root,
    prefix,
    async run(args = [], environment = {}) {
      const child = spawn('/bin/sh', [path.join(root, 'build/run-local.sh'), ...args], {
        cwd: temporary,
        env: {
          PATH: `${bin}:/usr/bin:/bin`,
          FIXTURE_LOG: log,
          FIXTURE_ROOT: root,
          FIXTURE_CHANNEL: channel,
          ANNOUNCE_IP: '127.0.0.1',
          ...environment,
        },
      });
      let output = '';
      child.stdout.on('data', chunk => { output += chunk; });
      child.stderr.on('data', chunk => { output += chunk; });
      const timer = setTimeout(() => child.kill('SIGKILL'), 10000);
      let status, signal;
      try { [status, signal] = await once(child, 'close'); }
      finally { clearTimeout(timer); }
      assert.equal(signal, null, output);
      const events = (await readFile(log, 'utf8')).trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
      return { status, output, events, server: events.find(event => event.command === 'server') };
    },
  };
}

test('local launcher help and invalid arguments have no setup side effects', async t => {
  const setup = await fixture(t, { installedOpenSsl: false, installedWeb: false });
  const help = await setup.run(['--help'], { ANNOUNCE_IP: '' });
  assert.equal(help.status, 0, help.output);
  assert.match(help.output, /--skip-web/);
  assert.deepEqual(help.events, []);
  for (const args of [['--unknown'], ['unexpected-argument'], ['--skip-web', 'unexpected-argument']]) {
    const result = await setup.run(args, { ANNOUNCE_IP: '' });
    assert.notEqual(result.status, 0, result.output);
    assert.deepEqual(result.events, []);
  }
});

test('local launcher pins Rust and refreshes locked web dependencies despite existing installations', async t => {
  const setup = await fixture(t, { channel: '9.88.7' });
  const result = await setup.run([], {
    RUSTC: '/wrong/homebrew/rustc',
    RUSTDOC: '/wrong/homebrew/rustdoc',
    RUSTUP_TOOLCHAIN: 'stable',
    OPENSSL_DIR: '/wrong/openssl',
    OPENSSL_STATIC: '0',
    PIP_CONSTRAINT: '/wrong/pip-constraints.txt',
    BIND_ADDR: '0.0.0.0',
  });
  assert.equal(result.status, 0, result.output);
  assert.deepEqual(result.server.args, ['run', '9.88.7', 'cargo', 'run', '--locked', '--bin', 'simplestChat']);
  assert.equal(result.server.cwd, setup.root);
  assert.equal(result.server.env.RUSTC, path.join(setup.root, 'toolchain/bin/rustc'));
  assert.equal(result.server.env.RUSTDOC, path.join(setup.root, 'toolchain/bin/rustdoc'));
  assert.equal(result.server.env.OPENSSL_DIR, setup.prefix);
  assert.equal(result.server.env.OPENSSL_STATIC, '1');
  assert.equal(result.server.env.PIP_CONSTRAINT, path.join(setup.root, 'build/pip-constraints.txt'));
  assert.ok(result.server.env.PKG_CONFIG_PATH.split(':').includes(path.join(setup.prefix, 'lib/pkgconfig')));
  assert.equal(result.server.env.BIND_ADDR, '127.0.0.1');
  assert.equal(result.server.env.PORT, '3000');
  assert.equal(result.server.env.MEDIA_WORKERS, '1');
  assert.equal(result.server.env.WEBRTC_SERVER_PORT_BASE, '40000');
  assert.equal(result.server.env.ALLOW_AD_HOC_ROOMS, 'true');
  assert.equal(result.events.filter(event => event.command === 'install-openssl').length, 0);
  const npm = result.events.filter(event => event.command === 'npm');
  assert.equal(npm.filter(event => event.args.includes('ci')).length, 1);
  assert.equal(npm.filter(event => event.args.includes('build')).length, 1);
  assert.deepEqual(npm.find(event => event.args.includes('ci')).args, ['--prefix', 'web', 'ci', '--ignore-scripts']);
  assert.ok(result.events.indexOf(npm.find(event => event.args.includes('ci'))) < result.events.indexOf(npm.find(event => event.args.includes('build'))));
  for (const event of npm.filter(event => event.args.includes('ci') || event.args.includes('build'))) {
    const prefix = event.args.indexOf('--prefix');
    assert.equal(prefix === -1 ? event.cwd : path.resolve(event.cwd, event.args[prefix + 1]), path.join(setup.root, 'web'));
  }
});

test('local launcher respects explicit network settings without macOS discovery', async t => {
  const setup = await fixture(t);
  const network = {
    PORT: '3207', ANNOUNCE_IP: '192.0.2.33', MEDIA_WORKERS: '2',
    WEBRTC_SERVER_PORT_BASE: '42107', ALLOW_AD_HOC_ROOMS: 'false',
  };
  const result = await setup.run(['--skip-web'], { FIXTURE_PLATFORM: 'Linux', ...network });
  assert.equal(result.status, 0, result.output);
  for (const [key, value] of Object.entries(network)) assert.equal(result.server.env[key], value);
  assert.equal(result.events.filter(event => ['route', 'ipconfig'].includes(event.command)).length, 0);
});

test('local launcher detects the macOS default interface and falls back to en0', async t => {
  for (const [routeFail, expected] of [['0', '192.0.2.7'], ['1', '192.0.2.20']]) {
    const setup = await fixture(t);
    const result = await setup.run(['--skip-web'], { ANNOUNCE_IP: '', FIXTURE_ROUTE_FAIL: routeFail });
    assert.equal(result.status, 0, result.output);
    assert.equal(result.server.env.ANNOUNCE_IP, expected);
  }
});

test('local launcher requires explicit announcement on non-macOS hosts', async t => {
  const setup = await fixture(t);
  const result = await setup.run(['--skip-web'], { ANNOUNCE_IP: '', FIXTURE_PLATFORM: 'Linux' });
  assert.notEqual(result.status, 0, result.output);
  assert.match(result.output, /ANNOUNCE_IP/);
  assert.equal(result.server, undefined);
});

test('local launcher installs missing prerequisites and skip-web omits both npm steps', async t => {
  const setup = await fixture(t, { installedOpenSsl: false, installedWeb: false });
  const result = await setup.run();
  assert.equal(result.status, 0, result.output);
  const installer = result.events.find(event => event.command === 'install-openssl');
  assert.deepEqual(installer.args, [setup.prefix]);
  const npm = result.events.filter(event => event.command === 'npm');
  assert.equal(npm.filter(event => event.args.includes('ci')).length, 1);
  assert.equal(npm.filter(event => event.args.includes('build')).length, 1);
  assert.ok(result.events.indexOf(npm.find(event => event.args.includes('ci'))) < result.events.indexOf(npm.find(event => event.args.includes('build'))));
  const skippedSetup = await fixture(t, { installedWeb: false });
  const skipped = await skippedSetup.run(['--skip-web']);
  assert.equal(skipped.status, 0, skipped.output);
  assert.equal(skipped.events.filter(event => event.command === 'npm').length, 0);

  const missingDist = await fixture(t, { installedDist: false });
  const rejected = await missingDist.run(['--skip-web']);
  assert.notEqual(rejected.status, 0, rejected.output);
  assert.match(rejected.output, /dist|build/i);
  assert.equal(rejected.server, undefined);
  assert.equal(rejected.events.filter(event => event.command === 'npm').length, 0);
});

test('local launcher requires the tracked pip constraint and both static OpenSSL archives', async t => {
  const missingConstraint = await fixture(t);
  await rm(path.join(missingConstraint.root, 'build/pip-constraints.txt'));
  const rejected = await missingConstraint.run(['--skip-web']);
  assert.notEqual(rejected.status, 0, rejected.output);
  assert.equal(rejected.server, undefined);
  assert.match(rejected.output, /pip-constraints/);

  const partialOpenSsl = await fixture(t);
  await rm(path.join(partialOpenSsl.prefix, 'lib/libcrypto.a'));
  const repaired = await partialOpenSsl.run(['--skip-web']);
  assert.equal(repaired.status, 0, repaired.output);
  assert.equal(repaired.events.filter(event => event.command === 'install-openssl').length, 1);
});

test('local launcher stops after setup/build failures and preserves server exit status', async t => {
  for (const phase of ['rustup-which', 'openssl-install', 'npm-ci', 'npm-build', 'cargo', 'server']) {
    const setup = await fixture(t, { installedOpenSsl: false, installedWeb: false });
    const result = await setup.run([], { FIXTURE_FAIL: phase, FIXTURE_FAILURE_CODE: '37' });
    assert.notEqual(result.status, 0, `${phase}: ${result.output}`);
    if (phase === 'server') assert.equal(result.status, 37, result.output);
    else assert.equal(result.server, undefined, `${phase} must stop before launching the server`);
  }
});
