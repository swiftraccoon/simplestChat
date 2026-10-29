import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import {
  mkdtempSync,
  mkdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import test from 'node:test';

const repository = fileURLToPath(new URL('../../', import.meta.url));
execFileSync('cargo', ['build', '--locked', '-q', '-p', 'xtask'], {
  cwd: repository,
  stdio: 'inherit',
});
const binary = path.join(repository, 'target/debug/xtask');

function fixture(t) {
  const root = realpathSync(
    mkdtempSync(path.join(tmpdir(), 'simplestchat-ci cli-')),
  );
  t.after(() => rmSync(root, { recursive: true, force: true }));
  for (const name of [
    'src/main.rs',
    'vendor/worker.cpp',
    'xtask/src/ci/mod.rs',
  ]) {
    mkdirSync(path.dirname(path.join(root, name)), { recursive: true });
    writeFileSync(path.join(root, name), 'original\n');
  }
  execFileSync('git', ['init', '--quiet'], { cwd: root });
  execFileSync('git', ['add', '.'], { cwd: root });
  const env = {
    ...process.env,
    ACT: 'true',
    LOCAL_CI_RUNNER_IMAGE: `docker.io/owned/runner@sha256:${'a'.repeat(64)}`,
    GITHUB_REF: 'refs/heads/main',
    GITHUB_EVENT_NAME: 'push',
    GITHUB_SHA: '1'.repeat(40),
  };
  delete env.CARGO_TARGET_DIR;
  return {
    root,
    env,
    run: (arguments_, overrides = {}) =>
      spawnSync(binary, ['ci', ...arguments_, '--root', root], {
        cwd: repository,
        encoding: 'utf8',
        env: { ...env, ...overrides },
      }),
  };
}

test('CI CLI binds an explicit canonical checkout and names the original receipt revision', (t) => {
  const f = fixture(t);
  const key = f.run(['key', '--scope', 'rust-test']);
  assert.equal(key.status, 0, key.stderr);
  assert.match(key.stdout, /^ci-verified-v1-rust-test-main-[a-f0-9]{64}\n$/);
  const directory = path.join(f.root, 'receipt');
  const options = [
    '--scope',
    'rust-test',
    '--key',
    key.stdout.trim(),
    '--directory',
    directory,
  ];
  assert.equal(f.run(['save', ...options]).status, 0);
  const result = f.run(['verify', ...options], { GITHUB_SHA: '2'.repeat(40) });
  assert.equal(result.status, 0, result.stderr);
  assert.equal(
    result.stdout,
    `Reused rust-test success for identical inputs; original revision: ${'1'.repeat(40)}\n`,
  );
  writeFileSync(path.join(f.root, 'src/main.rs'), 'changed worktree\n');
  const changed = f.run(['verify', ...options]);
  assert.equal(changed.status, 1);
  assert.match(changed.stderr, /CI cache rejected: ci_cache_inputs_changed/);
});

test('CI CLI rejects missing arguments, relative directories and symlinked cache roots', (t) => {
  const f = fixture(t);
  assert.equal(f.run(['key']).status, 2);
  assert.equal(f.run(['key', '--scope', 'unknown']).status, 2);
  const key = f.run(['key', '--scope', 'rust-test']).stdout.trim();
  const common = ['--scope', 'rust-test', '--key', key];
  const relative = f.run(['save', ...common, '--directory', 'relative-cache']);
  assert.equal(relative.status, 1);
  assert.match(relative.stderr, /ci_cache_directory/);
  const directory = path.join(f.root, 'actual-cache');
  mkdirSync(directory);
  const alias = path.join(f.root, 'cache-alias');
  symlinkSync(directory, alias);
  const symlink = f.run(['save', ...common, '--directory', alias]);
  assert.equal(symlink.status, 1);
  assert.match(symlink.stderr, /ci_cache_directory/);
});

test('CI CLI validates the complete restored receipt before acknowledging reuse', (t) => {
  const f = fixture(t);
  const key = f.run(['key', '--scope', 'rust-test']).stdout.trim();
  const directory = path.join(f.root, 'receipt');
  const options = [
    '--scope',
    'rust-test',
    '--key',
    key,
    '--directory',
    directory,
  ];
  assert.equal(f.run(['save', ...options]).status, 0);
  const filename = path.join(directory, 'receipt.json');
  const receipt = JSON.parse(readFileSync(filename, 'utf8'));
  writeFileSync(filename, JSON.stringify({ ...receipt, unrecognized: true }));
  const result = f.run(['verify', ...options]);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /invalid_tool_record_fields/);
  const refused = f.run([
    'save',
    '--scope',
    'codeql-rust',
    '--key',
    'unused',
    '--directory',
    directory,
  ]);
  assert.equal(refused.status, 1);
  assert.match(refused.stderr, /ci_cache_codeql_requires_database/);
});

test('CI key appends the same fixed output for each workflow scope without stdout or shell expansion', (t) => {
  const f = fixture(t);
  for (const scope of [
    'automation',
    'rust-lint',
    'rust-test',
    'backend',
    'browser-backend',
  ]) {
    const key = f.run(['key', '--scope', scope]);
    assert.equal(key.status, 0, key.stderr);
    const filename = path.join(f.root, `output ${scope} $(touch unowned); literal`);
    writeFileSync(filename, 'prior=preserved\n');
    const published = f.run(['key', '--scope', scope, '--github-output'], {
      GITHUB_OUTPUT: filename,
    });
    assert.equal(published.status, 0, published.stderr);
    assert.equal(published.stdout, '');
    assert.equal(published.stderr, '');
    assert.equal(readFileSync(filename, 'utf8'), `prior=preserved\nkey=${key.stdout}`);
  }
  const key = f.run(['key', '--scope', 'rust-test'], {
    GITHUB_EVENT_NAME: 'pull_request',
  });
  const filename = path.join(f.root, 'untrusted-output');
  writeFileSync(filename, '');
  const published = f.run(['key', '--scope', 'rust-test', '--github-output'], {
    GITHUB_OUTPUT: filename,
    GITHUB_EVENT_NAME: 'pull_request',
  });
  assert.equal(published.status, 0, published.stderr);
  assert.equal(readFileSync(filename, 'utf8'), `key=${key.stdout}`);
  assert.match(key.stdout, /^ci-verified-v1-rust-test-untrusted-[a-f0-9]{64}\n$/);
});

test('CI output mode refuses missing, unsafe or invalid requests without publishing or leaking paths', (t) => {
  const f = fixture(t);
  const output = path.join(f.root, 'private-output-sentinel');
  writeFileSync(output, 'prior=preserved\n');
  const alias = path.join(f.root, 'output-alias');
  symlinkSync(output, alias);
  const missing = path.join(f.root, 'private-missing-sentinel');
  for (const [filename, reason] of [
    ['', 'ci_cache_github_output_missing'],
    ['private-relative-sentinel', 'ci_cache_github_output_path'],
    [missing, 'ci_cache_github_output_open'],
    [alias, 'ci_cache_github_output_open'],
    [f.root, 'ci_cache_github_output_open'],
  ]) {
    const result = f.run(['key', '--scope', 'rust-test', '--github-output'], {
      GITHUB_OUTPUT: filename,
    });
    assert.equal(result.status, 1);
    assert.equal(result.stdout, '');
    assert.equal(result.stderr, `CI cache rejected: ${reason}\n`);
    assert.doesNotMatch(result.stderr, /private-.*-sentinel/);
    assert.equal(readFileSync(output, 'utf8'), 'prior=preserved\n');
  }
  for (const args of [
    ['save', '--scope', 'rust-test', '--github-output'],
    ['verify', '--scope', 'rust-test', '--github-output'],
    ['key', '--scope', 'unknown', '--github-output'],
    ['key', '--scope', 'rust-test', '--github-output', '--github-output'],
  ]) {
    const result = f.run(args, { GITHUB_OUTPUT: output });
    assert.equal(result.status, 2);
    assert.equal(result.stdout, '');
    assert.equal(readFileSync(output, 'utf8'), 'prior=preserved\n');
  }
});
