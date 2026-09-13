import assert from 'node:assert/strict';
import { readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const read = filename => readFileSync(path.join(root, filename), 'utf8');
const dockerfile = read('Dockerfile').replace(/\\\r?\n\s*/g, ' ');
const webStage = dockerfile.split(/\bAS web-builder\s*\n/i)[1]?.split(/^FROM /m)[0];
const ignoreRules = read('.dockerignore').split(/\r?\n/).map(line => line.trim())
  .filter(line => line && !line.startsWith('#'));

// Deliberately support only this file's exact paths, trailing /** and single
// filename-extension wildcards. Docker matches a rule against parent paths too:
// https://github.com/moby/patternmatcher/blob/v0.6.0/patternmatcher.go
// Refuse new syntax rather than pretending this is a complete Docker matcher.
function excluded(filename, rules = ignoreRules) {
  const parts = filename.split('/');
  const candidates = parts.map((_, index) => parts.slice(0, index + 1).join('/'));
  let ignored = false;
  for (const rule of rules) {
    const exception = rule.startsWith('!');
    const pattern = (exception ? rule.slice(1) : rule).replace(/\/$/, '');
    const extension = pattern.match(/^([\w./-]+)\/\*\.(\w+)$/);
    assert.ok(pattern === '**' || /^[\w./-]+(?:\/\*\*)?$/.test(pattern) || extension,
      `Unsupported Docker context pattern: ${pattern}`);
    const matches = candidate => pattern === '**'
      || (pattern.endsWith('/**') && candidate.startsWith(pattern.slice(0, -2)))
      || (extension && path.posix.dirname(candidate) === extension[1]
        && candidate.endsWith(`.${extension[2]}`))
      || candidate === pattern;
    if (candidates.some(matches)) ignored = !exception;
  }
  return ignored;
}

// Inspect the deliberately simple, local COPY syntax used by this stage.
// This is an input-closure guard, not a Dockerfile or dockerignore interpreter;
// the production image build and startup smoke remain authoritative.
function copiedDestination(filename) {
  assert.ok(webStage, 'Dockerfile must retain its web-builder stage');
  assert.match(webStage, /^WORKDIR \/web$/m);
  const beforeBuild = webStage.split(/^RUN npm run build$/m)[0];
  for (const line of beforeBuild.split('\n').filter(value => value.startsWith('COPY '))) {
    const values = line.slice(5).trim().split(/\s+/);
    const destination = values.pop();
    assert.ok(destination);
    assert.ok(values.every(value => /^[\w./-]+$/.test(value)),
      'Extend this guard explicitly when changing web-builder COPY syntax');
    for (const source of values) {
      const directory = statSync(path.join(root, source)).isDirectory();
      if (directory && filename.startsWith(`${source}/`)) {
        return path.posix.resolve('/web', destination, filename.slice(source.length + 1));
      }
      if (!directory && source === filename) {
        return path.posix.resolve('/web', destination,
          destination.endsWith('/') ? path.posix.basename(filename) : '');
      }
    }
  }
  return undefined;
}

test('web-builder copies the complete production frontend input set', () => {
  const projects = JSON.parse(read('web/tsconfig.json')).references
    .map(reference => path.posix.join('web', reference.path));
  const required = [
    'web/package.json', 'web/package-lock.json', 'web/tsconfig.json', ...projects,
    'web/vite.config.ts', 'web/index.html', 'web/src/main.ts',
    'web/scripts/check-bundle.mjs', 'web/bundle-budget.json',
    'web/public/help.html', 'web/public/help.css',
  ];
  for (const filename of required) {
    assert.equal(copiedDestination(filename), `/${filename}`,
      `${filename} must be copied to its expected web-builder path`);
  }
  const appProject = JSON.parse(read('web/tsconfig.tools.json')).extends;
  assert.equal(copiedDestination(path.posix.join('web', appProject)),
    path.posix.resolve('/web', appProject));
  assert.match(webStage, /^RUN npm ci --ignore-scripts$/m);
  assert.match(webStage, /^RUN npm run build$/m);
  assert.equal(JSON.parse(read('web/package.json')).scripts.build,
    'npm run typecheck && vite build && npm run check:bundle');
});

test('container context explicitly admits build configs and help without broadening web access', () => {
  assert.equal(ignoreRules[0], '**', 'Container context must remain deny by default');
  const webRules = ignoreRules.filter(rule => rule.replace(/^!/, '').startsWith('web'));
  assert.deepEqual(webRules.toSorted(), [
    '!web/', 'web/**', '!web/package.json', '!web/package-lock.json',
    '!web/tsconfig.json', '!web/tsconfig.app.json', '!web/tsconfig.tools.json',
    '!web/vite.config.ts', '!web/index.html', '!web/bundle-budget.json',
    '!web/scripts/', 'web/scripts/**', '!web/scripts/check-bundle.mjs',
    '!web/public/', 'web/public/**', '!web/public/help.html', '!web/public/help.css',
    '!web/src/', 'web/src/**', '!web/src/*.ts', '!web/src/*.css',
  ].toSorted());
  for (const [index, rule] of ignoreRules.entries()) {
    if (rule.startsWith('!') && rule.endsWith('/') && rule !== '!vendor/') {
      assert.equal(ignoreRules[index + 1], `${rule.slice(1)}**`,
        `${rule} must re-exclude descendants before admitting required files`);
    }
  }
});

test('context matching covers parent inheritance and last-rule precedence', () => {
  // Mirrors the official matcher regression: a directory exception admits a
  // descendant despite an earlier **. The traversal exclusion must reverse it.
  assert.equal(excluded('web/private.env', ['**', '!web/']), false);
  assert.equal(excluded('web/private.env', ['**', '!web/', 'web/**']), true);
  assert.equal(excluded('web/index.html', ['**', '!web/', 'web/**', '!web/index.html']), false);
  assert.equal(excluded('web', ['**', '!web/', 'web/**']), false);
  assert.throws(() => excluded('web/index.html', ['web/[ab].html']), /Unsupported/);
});

test('context excludes local-only descendants while retaining production inputs', () => {
  for (const filename of [
    '.env', 'CLAUDE.md', 'results/local/report.json',
    'build/CLAUDE.md', 'build/private.env', 'src/CLAUDE.md', 'src/auth/private.env',
    'src/media/CLAUDE.md', 'src/room/notes.md', 'src/signaling/private.env',
    'migrations/private.env', 'load_tests/notes.md', 'load_tests/bin/CLAUDE.md',
    'load_tests/clients/private.env', 'web/CLAUDE.md', 'web/.env',
    'web/node_modules/example/index.js', 'web/e2e/results/report.json',
    'web/scripts/CLAUDE.md', 'web/scripts/local-only.mjs',
    'web/public/private.env', 'web/public/unlisted.html', 'web/src/CLAUDE.md',
  ]) assert.equal(excluded(filename), true, `${filename} must stay outside the build context`);
  for (const filename of [
    'Dockerfile', '.dockerignore', 'Cargo.toml', 'Cargo.lock',
    'build/pip-constraints.txt', 'build/install-openssl.sh',
    'src/main.rs', 'src/auth/mod.rs', 'src/media/mod.rs', 'src/room/mod.rs',
    'src/signaling/mod.rs', 'migrations/001_initial.sql',
    'load_tests/bin/load_test.rs', 'load_tests/clients/example.rs',
    'web/package.json', 'web/package-lock.json', 'web/tsconfig.json',
    'web/tsconfig.app.json', 'web/tsconfig.tools.json', 'web/vite.config.ts',
    'web/index.html', 'web/bundle-budget.json', 'web/scripts/check-bundle.mjs',
    'web/public/help.html', 'web/public/help.css', 'web/src/main.ts', 'web/src/style.css',
    'vendor/mediasoup-sys-0.17.0/subprojects/packagefiles/abseil-cpp/meson.build',
  ]) assert.equal(excluded(filename), false, `${filename} must remain available to the build`);
});
