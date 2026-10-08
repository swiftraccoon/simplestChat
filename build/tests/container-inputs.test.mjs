import assert from 'node:assert/strict';
import { globSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const read = filename => readFileSync(path.join(root, filename), 'utf8');
const dockerfile = read('Dockerfile').replace(/\\\r?\n\s*/g, ' ');
const webStage = dockerfile.split(/\bAS web-builder\s*\n/i)[1]?.split(/^FROM /m)[0];
const ignoreRules = read('.dockerignore').split(/\r?\n/).map(line => line.trim())
  .filter(line => line && !line.startsWith('#'));
const pwaFiles = ['manifest.webmanifest', 'sw.js',
  ...JSON.parse(read('web/public/manifest.webmanifest')).icons.map(icon => {
    assert.match(icon.src, /^\/[\w-]+\.png$/);
    return icon.src.slice(1);
  })].map(filename => `web/public/${filename}`);

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

// Check every explicit local COPY source, including native build helpers. This
// intentionally rejects new syntax until the input guard understands it.
function verifyLocalCopySources(rules = ignoreRules) {
  for (const line of dockerfile.split('\n').filter(value => value.startsWith('COPY '))) {
    const values = line.slice(5).trim().split(/\s+/);
    if (values[0].startsWith('--from=')) continue;
    assert.ok(values.length >= 2 && values.every(value => /^[\w./*-]+$/.test(value)),
      'Extend the local COPY input guard explicitly when introducing new syntax');
    for (const source of values.slice(0, -1)) {
      const filenames = source.includes('*') ? globSync(source, { cwd: root }) : [source];
      assert.ok(filenames.length > 0, `${source} must match a build input`);
      for (const filename of filenames) {
        assert.ok(statSync(path.join(root, filename)), `${filename} must exist`);
        assert.equal(excluded(filename, rules), false,
          `${filename} is explicitly copied but excluded from the build context`);
      }
    }
  }
}

// RUN bind inputs need the same context closure as COPY, but must stay
// read-only and outside committed image layers. Keep this deliberately narrow;
// only the exact reviewed projection may come from another build stage.
function verifyLocalRunSources(rules = ignoreRules, recipe = dockerfile) {
  const sources = [];
  for (const line of recipe.split('\n').filter(value => value.startsWith('RUN '))) {
    const tokens = line.slice(4).trim().split(/\s+/);
    while (tokens[0]?.startsWith('--mount=')) {
      const fields = tokens.shift().slice('--mount='.length).split(',');
      const options = new Map(fields.map(field => {
        const parts = field.split('=');
        assert.ok(parts.length <= 2, 'Unsupported RUN mount option');
        return [parts[0], parts[1] ?? true];
      }));
      assert.equal(options.size, fields.length, 'Duplicate RUN mount option');
      assert.equal(options.get('type'), 'bind', 'RUN review inputs must use bind mounts');
      assert.equal(options.get('readonly'), true, 'RUN review inputs must be read-only');
      if (options.has('from')) {
        assert.deepEqual(Object.fromEntries(options), {
          type: 'bind', from: 'image-review-inputs', source: '/image-exceptions.json',
          target: '/tmp/simplestchat-image-exceptions.json', readonly: true,
        }, 'Only the exact reviewed projection mount may cross stages');
        sources.push('image-review-inputs:/image-exceptions.json');
        continue;
      }
      assert.deepEqual([...options.keys()].toSorted(), ['readonly', 'source', 'target', 'type']);
      const source = options.get('source');
      const target = options.get('target');
      assert.ok(typeof source === 'string' && /^[\w./-]+$/.test(source)
        && path.posix.normalize(source) === source && !source.startsWith('/')
        && !source.startsWith('../'), 'RUN source must be an exact context-relative file');
      assert.ok(typeof target === 'string' && /^\/[\w/-]+\.json$/.test(target),
        'RUN target must be an absolute JSON file');
      assert.ok(statSync(path.join(root, source)).isFile(), 'RUN source must be a regular file');
      assert.equal(excluded(source, rules), false,
        `${source} is explicitly mounted but excluded from the build context`);
      sources.push(source);
    }
  }
  return sources;
}

test('all explicit local COPY inputs are admitted by the Docker context', () => {
  verifyLocalCopySources();
  assert.throws(() => verifyLocalCopySources(ignoreRules.filter(rule => rule !== '!build/security_elf.py')),
    /security_elf\.py is explicitly copied but excluded/);
  assert.throws(() => verifyLocalCopySources(ignoreRules.filter(rule => rule !== '!web/scripts/mediasoup-runtime.mjs')),
    /mediasoup-runtime\.mjs is explicitly copied but excluded/);
  assert.throws(() => verifyLocalCopySources(ignoreRules.filter(rule => rule !== '!security/exceptions.json')),
    /exceptions\.json is explicitly copied but excluded/);
});

test('both Fedora stages mount only admitted read-only public policy files', () => {
  assert.deepEqual(verifyLocalRunSources(), [
    'image-review-inputs:/image-exceptions.json', 'security/image-policy.json',
    'image-review-inputs:/image-exceptions.json', 'security/image-policy.json',
  ]);
  assert.throws(() => verifyLocalRunSources(ignoreRules.filter(rule => rule !== '!security/image-policy.json')),
    /explicitly mounted but excluded/);
  assert.throws(() => verifyLocalRunSources(ignoreRules, dockerfile.replace(',readonly', ',readwrite')),
    /read-only/);
  assert.throws(() => verifyLocalRunSources(ignoreRules,
    dockerfile.replace('source=security/image-policy.json', 'source=security')),
  /RUN source must be a regular file/);
  for (const [before, after] of [
    ['from=image-review-inputs', 'from=unreviewed-stage'],
    ['source=/image-exceptions.json', 'source=/other.json'],
    ['target=/tmp/simplestchat-image-exceptions.json', 'target=/tmp/other.json'],
  ]) {
    assert.throws(() => verifyLocalRunSources(ignoreRules, dockerfile.replace(before, after)),
      /Only the exact reviewed projection mount may cross stages/);
  }
  assert.throws(() => verifyLocalRunSources(ignoreRules,
    dockerfile.replace('type=bind', 'type=secret')), /RUN review inputs must use bind mounts/);
  const securityRules = ignoreRules.filter(rule => rule.replace(/^!/, '').startsWith('security'));
  assert.deepEqual(securityRules.toSorted(), [
    '!security/', 'security/**', '!security/exceptions.json', '!security/image-policy.json',
    '!security/runtime/', 'security/runtime/**', '!security/runtime/nsswitch.conf',
  ].toSorted());
});

test('web-builder copies the complete production frontend input set', () => {
  const projects = JSON.parse(read('web/tsconfig.json')).references
    .map(reference => path.posix.join('web', reference.path));
  const required = [
    'web/package.json', 'web/package-lock.json', 'web/tsconfig.json', ...projects,
    'web/vite.config.ts', 'web/index.html', 'web/src/main.ts',
    'web/scripts/check-bundle.mjs', 'web/scripts/mediasoup-runtime.mjs', 'web/bundle-budget.json',
    'web/public/help.html', 'web/public/help.css', ...pwaFiles,
  ];
  for (const filename of required) {
    assert.equal(copiedDestination(filename), `/${filename}`,
      `${filename} must be copied to its expected web-builder path`);
    assert.equal(excluded(filename), false,
      `${filename} must be admitted to the production build context`);
  }
  const appProject = JSON.parse(read('web/tsconfig.tools.json')).extends;
  assert.equal(copiedDestination(path.posix.join('web', appProject)),
    path.posix.resolve('/web', appProject));
  assert.match(webStage, /^RUN npm ci --ignore-scripts$/m);
  assert.match(webStage, /^RUN npm run build$/m);
  assert.equal(JSON.parse(read('web/package.json')).scripts.build,
    'npm run typecheck && vite build && npm run check:bundle');
});

test('container context explicitly admits build configs, help and PWA files without broadening web access', () => {
  assert.equal(ignoreRules[0], '**', 'Container context must remain deny by default');
  const webRules = ignoreRules.filter(rule => rule.replace(/^!/, '').startsWith('web'));
  assert.deepEqual(webRules.toSorted(), [
    '!web/', 'web/**', '!web/package.json', '!web/package-lock.json',
    '!web/tsconfig.json', '!web/tsconfig.app.json', '!web/tsconfig.tools.json',
    '!web/vite.config.ts', '!web/index.html', '!web/bundle-budget.json',
    '!web/scripts/', 'web/scripts/**', '!web/scripts/check-bundle.mjs',
    '!web/scripts/mediasoup-runtime.mjs',
    '!web/public/', 'web/public/**', '!web/public/help.html', '!web/public/help.css',
    '!web/public/manifest.webmanifest', '!web/public/sw.js',
    '!web/public/icon-192.png', '!web/public/icon-512.png',
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
    'security/private.env', 'security/notes.json', 'security/runtime/private.env',
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
    'security/exceptions.json', 'security/image-policy.json', 'security/runtime/nsswitch.conf',
    'src/main.rs', 'src/auth/mod.rs', 'src/media/mod.rs', 'src/room/mod.rs',
    'src/signaling/mod.rs', 'migrations/001_initial.sql',
    'load_tests/bin/load_test.rs', 'load_tests/clients/example.rs',
    'web/package.json', 'web/package-lock.json', 'web/tsconfig.json',
    'web/tsconfig.app.json', 'web/tsconfig.tools.json', 'web/vite.config.ts',
    'web/index.html', 'web/bundle-budget.json', 'web/scripts/check-bundle.mjs',
    'web/scripts/mediasoup-runtime.mjs',
    'web/public/help.html', 'web/public/help.css', ...pwaFiles,
    'web/src/main.ts', 'web/src/style.css',
    'vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/abseil-cpp/meson.build',
  ]) assert.equal(excluded(filename), false, `${filename} must remain available to the build`);
});
