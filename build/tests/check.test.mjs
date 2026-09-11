import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmod, copyFile, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

const checker = new URL('../check.sh', import.meta.url);
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

// Run the actual wrapper in an isolated checkout. Every command that could
// build, install, test, or start a service is intercepted before execution.
const fakeTool = String.raw`
const fs = require('node:fs');
const path = require('node:path');
const [command, ...args] = process.argv.slice(2);
const env = process.env;
fs.appendFileSync(env.FIXTURE_LOG, JSON.stringify({
  command, args, cwd: process.cwd(),
  env: Object.fromEntries(['PATH', 'RUSTC', 'RUSTDOC', 'OPENSSL_DIR',
    'OPENSSL_STATIC', 'PKG_CONFIG_PATH', 'PIP_CONSTRAINT', 'RUSTDOCFLAGS']
    .map(key => [key, env[key]])),
}) + '\n');
let phase;
if (command === 'npm') {
  if (args[0] !== '--prefix' || args[1] !== 'web') process.exit(91);
  phase = 'npm-' + (args[2] === 'run' ? args[3] : args[2]);
  if (!['npm-lint', 'npm-format:check', 'npm-test', 'npm-build'].includes(phase)) process.exit(92);
} else if (command === 'rustup') {
  if (args[0] === 'which') {
    if (args[1] !== '--toolchain' || args[2] !== env.FIXTURE_CHANNEL ||
        !['rustc', 'rustdoc'].includes(args[3])) process.exit(93);
    phase = 'rustup-which-' + args[3];
    if (env.FIXTURE_FAIL !== phase) console.log(path.join(env.FIXTURE_TOOLCHAIN_BIN, args[3]));
  } else if (args[0] === 'run') {
    if (args[1] !== env.FIXTURE_CHANNEL || args[2] !== 'cargo') process.exit(94);
    phase = 'cargo-' + args[3];
    if (!['cargo-fmt', 'cargo-clippy', 'cargo-doc'].includes(phase)) process.exit(95);
  } else process.exit(96);
} else if (command === 'sh' || command === 'bash') {
  // A shell accepts only one script after -n. Passing several does not check
  // all of them, so reject that mistake even if a real shell would return zero.
  if (args.length !== 2 || args[0] !== '-n' || !fs.statSync(args[1]).isFile()) process.exit(97);
  phase = command + ':' + args[1];
} else if (command === 'node') {
  if (args[0] !== '--test') process.exit(98);
  phase = 'helper-tests';
} else if (command === 'shellcheck') {
  if (args.length === 0 || args.some(file => !fs.statSync(file).isFile())) process.exit(98);
  phase = 'shellcheck';
} else {
  console.error('Unexpected command: ' + command);
  process.exit(99);
}
if (env.FIXTURE_FAIL === phase) process.exit(Number(env.FIXTURE_FAILURE_CODE || 37));
`;

async function fixture(t, { installedWeb = true, installedOpenSsl = true, omittedTool, channel = '9.88.7' } = {}) {
  const temporary = await realpath(await mkdtemp(path.join(os.tmpdir(), 'simplestchat-check-test.')));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const root = path.join(temporary, 'checkout with spaces');
  const bin = path.join(temporary, 'fake commands');
  const toolchainBin = path.join(root, 'pinned toolchain', 'bin');
  const dispatcher = path.join(temporary, 'fake-tool.cjs');
  const log = path.join(temporary, 'commands.jsonl');
  const helperNames = ['check.sh', 'helper with spaces.sh', 'run-local.sh', 'with-test-server.sh'];
  await Promise.all([
    mkdir(path.join(root, 'build/tests'), { recursive: true }),
    mkdir(path.join(root, 'load_tests'), { recursive: true }),
    mkdir(path.join(root, 'web'), { recursive: true }),
    mkdir(toolchainBin, { recursive: true }),
    mkdir(bin),
    writeFile(dispatcher, fakeTool),
    writeFile(log, ''),
  ]);
  await copyFile(checker, path.join(root, 'build/check.sh'));
  for (const name of helperNames.filter(name => name !== 'check.sh')) {
    await writeFile(path.join(root, 'build', name), '#!/bin/sh\nexit 99 # must never execute\n');
  }
  await Promise.all([
    writeFile(path.join(root, 'rust-toolchain.toml'), `[toolchain]\nchannel = "${channel}"\n`),
    writeFile(path.join(root, 'build/pip-constraints.txt'), '# fixture only\n'),
    writeFile(path.join(root, 'build/tests/first.test.mjs'), 'throw new Error("must never execute");\n'),
    writeFile(path.join(root, 'build/tests/second.test.mjs'), 'throw new Error("must never execute");\n'),
    writeFile(path.join(root, 'load_tests/benchmark-local.test.mjs'), 'throw new Error("must never execute");\n'),
  ]);
  if (installedWeb) await mkdir(path.join(root, 'web/node_modules'));
  const prefix = path.join(root, 'target/openssl-3.5.8');
  async function installFixtureOpenSsl(directory) {
    await mkdir(path.join(directory, 'lib'), { recursive: true });
    await Promise.all(['libssl.a', 'libcrypto.a'].map(name => writeFile(path.join(directory, 'lib', name), 'fixture only\n')));
  }
  if (installedOpenSsl) await installFixtureOpenSsl(prefix);
  for (const command of ['npm', 'rustup', 'node', 'sh', 'bash', 'shellcheck']) {
    if (command === omittedTool) continue;
    const executable = path.join(bin, command);
    await writeFile(executable, `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(dispatcher)} ${quote(command)} "$@"\n`);
    await chmod(executable, 0o755);
  }
  // PATH deliberately excludes system command directories: a missing fake npm,
  // node, or rustup can never fall back to a real tool installed on the host.
  for (const command of ['dirname', 'sed']) await symlink(`/usr/bin/${command}`, path.join(bin, command));
  return {
    root, prefix, bin, toolchainBin, helperNames, installFixtureOpenSsl,
    async run(args = [], environment = {}) {
      await writeFile(log, '');
      const result = spawnSync('/bin/sh', [path.join(root, 'build/check.sh'), ...args], {
        cwd: temporary,
        env: {
          PATH: bin, LC_ALL: 'C', FIXTURE_LOG: log,
          FIXTURE_CHANNEL: channel, FIXTURE_TOOLCHAIN_BIN: toolchainBin,
          ...environment,
        },
        encoding: 'utf8', timeout: 10_000,
      });
      assert.equal(result.error, undefined, result.stderr);
      assert.equal(result.signal, null, result.stderr);
      const events = (await readFile(log, 'utf8')).split('\n').filter(Boolean).map(line => JSON.parse(line));
      return { status: result.status, output: result.stdout + result.stderr, events };
    },
  };
}

test('quality checker help and invalid arguments execute no tools', async t => {
  const setup = await fixture(t, { installedWeb: false, installedOpenSsl: false });
  for (const argument of ['--help', '-h']) {
    const result = await setup.run([argument]);
    assert.equal(result.status, 0, result.output);
    assert.match(result.output, /--web.*--rust.*--helpers/);
    assert.deepEqual(result.events, []);
  }
  for (const args of [['--unknown'], ['web'], ['--web', '--rust'], ['--help', 'extra']]) {
    const result = await setup.run(args);
    assert.equal(result.status, 2, result.output);
    assert.match(result.output, /Usage:/);
    assert.deepEqual(result.events, []);
  }
});

test('web quality group checks lint, format, tests and build without installing dependencies', async t => {
  const setup = await fixture(t);
  const result = await setup.run(['--web']);
  assert.equal(result.status, 0, result.output);
  assert.deepEqual(result.events.map(event => [event.command, ...event.args]), [
    ['npm', '--prefix', 'web', 'run', 'lint'],
    ['npm', '--prefix', 'web', 'run', 'format:check'],
    ['npm', '--prefix', 'web', 'test'],
    ['npm', '--prefix', 'web', 'run', 'build'],
  ]);
  assert.ok(result.events.every(event => event.cwd === setup.root));
});

test('Rust quality group pins tools, native inputs and warning-denying Clippy/documentation', async t => {
  const setup = await fixture(t);
  const result = await setup.run(['--rust'], {
    RUSTC: '/wrong/rustc', RUSTDOC: '/wrong/rustdoc', RUSTUP_TOOLCHAIN: 'stable',
    PKG_CONFIG_PATH: '/wrong/pkgconfig', OPENSSL_STATIC: '0', PIP_CONSTRAINT: '/wrong/pip.txt',
    RUSTDOCFLAGS: '--cfg quality_fixture',
  });
  assert.equal(result.status, 0, result.output);
  assert.deepEqual(result.events.map(event => event.args), [
    ['which', '--toolchain', '9.88.7', 'rustc'],
    ['which', '--toolchain', '9.88.7', 'rustdoc'],
    ['run', '9.88.7', 'cargo', 'fmt', '--all', '--', '--check'],
    ['run', '9.88.7', 'cargo', 'clippy', '--locked', '--all-targets', '--all-features', '--no-deps', '--', '-D', 'warnings'],
    ['run', '9.88.7', 'cargo', 'doc', '--locked', '--all-features', '--no-deps', '--document-private-items'],
  ]);
  assert.ok(result.events.every(event => event.command === 'rustup' && event.cwd === setup.root));
  for (const event of result.events.filter(event => event.args[0] === 'run')) {
    assert.equal(event.env.RUSTC, path.join(setup.toolchainBin, 'rustc'));
    assert.equal(event.env.RUSTDOC, path.join(setup.toolchainBin, 'rustdoc'));
    assert.equal(event.env.PATH, `${setup.toolchainBin}:${setup.bin}`);
    assert.equal(event.env.OPENSSL_DIR, setup.prefix);
    assert.equal(event.env.OPENSSL_STATIC, '1');
    assert.equal(event.env.PKG_CONFIG_PATH, path.join(setup.prefix, 'lib/pkgconfig'));
    assert.equal(event.env.PIP_CONSTRAINT, path.join(setup.root, 'build/pip-constraints.txt'));
  }
  assert.equal(result.events.at(-1).env.RUSTDOCFLAGS,
    '--cfg quality_fixture -D warnings');
});

test('Rust checks respect an inherited static OpenSSL prefix containing spaces', async t => {
  const setup = await fixture(t, { installedOpenSsl: false });
  const prefix = path.join(setup.root, 'custom static OpenSSL');
  await setup.installFixtureOpenSsl(prefix);
  const result = await setup.run(['--rust'], { OPENSSL_DIR: prefix });
  assert.equal(result.status, 0, result.output);
  for (const event of result.events.filter(event => event.args[0] === 'run')) {
    assert.equal(event.env.OPENSSL_DIR, prefix);
    assert.equal(event.env.PKG_CONFIG_PATH, path.join(prefix, 'lib/pkgconfig'));
  }
});

test('helper checks syntax-check every shell file individually before running only helper tests', async t => {
  const setup = await fixture(t, { installedWeb: false, installedOpenSsl: false });
  const result = await setup.run(['--helpers']);
  assert.equal(result.status, 0, result.output);
  assert.deepEqual(result.events.filter(event => event.command === 'sh').map(event => event.args), [
    ['-n', 'build/check.sh'], ['-n', 'build/run-local.sh'],
  ]);
  assert.deepEqual(result.events.filter(event => event.command === 'bash').map(event => event.args),
    setup.helperNames.map(name => ['-n', `build/${name}`]));
  assert.deepEqual(result.events.find(event => event.command === 'shellcheck').args,
    setup.helperNames.map(name => `build/${name}`));
  assert.deepEqual(result.events.at(-1).args, [
    '--test', 'build/tests/first.test.mjs', 'build/tests/second.test.mjs', 'load_tests/benchmark-local.test.mjs',
  ]);
  assert.equal(result.events.at(-1).command, 'node');
  assert.ok(result.events.every(event => ['sh', 'bash', 'shellcheck', 'node'].includes(event.command) && event.cwd === setup.root));
});

test('default quality check runs web, Rust and helper groups in order', async t => {
  const setup = await fixture(t);
  const result = await setup.run();
  assert.equal(result.status, 0, result.output);
  assert.deepEqual(result.events.map(event => event.command), [
    'npm', 'npm', 'npm', 'npm', 'rustup', 'rustup', 'rustup', 'rustup', 'rustup',
    'sh', 'sh', ...setup.helperNames.map(() => 'bash'), 'shellcheck', 'node',
  ]);
});

test('quality checks reject missing dependencies and native archives without installing anything', async t => {
  const setup = await fixture(t, { installedWeb: false, installedOpenSsl: false });
  const web = await setup.run(['--web']);
  assert.equal(web.status, 2, web.output);
  assert.match(web.output, /Install web dependencies first/);
  assert.deepEqual(web.events, []);
  const rust = await setup.run(['--rust']);
  assert.equal(rust.status, 2, rust.output);
  assert.match(rust.output, /Static OpenSSL is missing/);
  assert.ok(rust.events.every(event => event.command === 'rustup' && event.args[0] === 'which'));
});

test('quality checks reject a missing toolchain pin before invoking rustup', async t => {
  const setup = await fixture(t);
  await writeFile(path.join(setup.root, 'rust-toolchain.toml'), '[toolchain]\n');
  const result = await setup.run(['--rust']);
  assert.equal(result.status, 2, result.output);
  assert.match(result.output, /Cannot read the pinned Rust toolchain/);
  assert.deepEqual(result.events, []);
});

for (const [omittedTool, group] of [['npm', '--web'], ['rustup', '--rust'], ['sh', '--helpers'], ['bash', '--helpers'], ['shellcheck', '--helpers'], ['node', '--helpers']]) {
  test(`quality checks fail when ${omittedTool} is unavailable without reaching another group`, async t => {
    const setup = await fixture(t, { omittedTool });
    const result = await setup.run([group]);
    assert.equal(result.status, 127, result.output);
    assert.match(result.output, new RegExp(`${omittedTool}: ((?:command )?not found|No such file)`));
    assert.ok(!result.events.some(event => event.command === omittedTool));
  });
}

for (const [phase, lastCommand] of [
  ['npm-lint', 'npm'], ['npm-format:check', 'npm'], ['npm-test', 'npm'], ['npm-build', 'npm'],
  ['rustup-which-rustc', 'rustup'], ['rustup-which-rustdoc', 'rustup'],
  ['cargo-fmt', 'rustup'], ['cargo-clippy', 'rustup'], ['cargo-doc', 'rustup'],
  ['sh:build/run-local.sh', 'sh'], ['bash:build/helper with spaces.sh', 'bash'], ['shellcheck', 'shellcheck'], ['helper-tests', 'node'],
]) {
  test(`default quality check stops immediately and preserves ${phase} failure status`, async t => {
    const setup = await fixture(t);
    const result = await setup.run([], { FIXTURE_FAIL: phase, FIXTURE_FAILURE_CODE: '43' });
    assert.equal(result.status, 43, result.output);
    assert.equal(result.events.at(-1).command, lastCommand);
    const event = result.events.at(-1);
    const actualPhase = event.command === 'npm' ? `npm-${event.args[2] === 'run' ? event.args[3] : event.args[2]}`
      : event.command === 'rustup' ? event.args[0] === 'which' ? `rustup-which-${event.args[3]}` : `cargo-${event.args[3]}`
      : event.command === 'node' ? 'helper-tests' : event.command === 'shellcheck' ? 'shellcheck' : `${event.command}:${event.args[1]}`;
    assert.equal(actualPhase, phase, 'no command may run after the injected failure');
  });
}
