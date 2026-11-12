import assert from 'node:assert/strict';
import { existsSync, globSync, readFileSync, statSync } from 'node:fs';
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
// The favicon and stylesheet icons are files: the content security policy admits no data: images.
const iconFiles = ['web/public/favicon.svg',
  ...globSync('web/public/icons/*.svg', { cwd: root })];
assert.ok(iconFiles.length > 1, 'the stylesheet icons are static files');

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
    '!security/http-policy.json',
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
    'web/public/help.html', 'web/public/help.css', ...pwaFiles, ...iconFiles,
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
    '!web/public/favicon.svg',
    '!web/public/icons/', 'web/public/icons/**', '!web/public/icons/*.svg',
    '!web/public/fonts/', 'web/public/fonts/**', '!web/public/fonts/outfit-latin.woff2',
    '!web/public/fonts/Outfit-OFL.txt',
    '!web/src/', 'web/src/**', '!web/src/*.ts', '!web/src/*.css',
    '!web/src/screens/', 'web/src/screens/**', '!web/src/screens/*.ts',
    '!web/src/chat/', 'web/src/chat/**', '!web/src/chat/*.ts',
    '!web/src/room/', 'web/src/room/**', '!web/src/room/*.ts',
    '!web/src/community/', 'web/src/community/**', '!web/src/community/*.ts',
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

test('every Rust/browser source and migration in the repository reaches the build context', () => {
  // The context admits source directories one by one, so a new directory needs its own
  // rules: the Rust CI jobs build from the checkout and pass while only the image build
  // sees the hole (CI runs 39295831051 and 39302090481).
  const sources = ['build.rs', 'ops/src/**/*.rs', 'src/**/*.rs', 'load_tests/src/**/*.rs', 'xtask/src/**/*.rs', 'migrations/*.sql',
    'crates/*/src/**/*.rs', 'web/src/**/*.ts', 'web/src/**/*.css']
    .flatMap(pattern => globSync(pattern, { cwd: root }));
  assert.ok(sources.length > 100, 'the source globs must find the crate sources');
  for (const filename of sources) {
    assert.equal(excluded(filename.split(path.sep).join('/')), false,
      `${filename} must reach the build context: admit its directory in .dockerignore`);
  }
});

test('the builder stage loads every workspace member before warming dependencies', () => {
  // Cargo loads every member manifest, so a member the builder does not copy and stub
  // fails only the hosted image build while every checkout-based Rust job passes.
  const builder = dockerfile.split(/\bAS builder\s*\n/i)[1]?.split(/^FROM /m)[0];
  assert.ok(builder, 'Dockerfile must retain its builder stage');
  const warm = builder.indexOf('auditable build');
  assert.ok(warm > 0, 'the builder warms dependencies with a stub build');
  const workspace = read('Cargo.toml').split(/^\[/m).find(table => table.startsWith('workspace]'));
  const list = workspace?.match(/^members\s*=\s*\[([^\]]*)\]/m)?.[1] ?? '';
  const members = [...list.matchAll(/"([^"]+)"/g)].map(match => match[1]);
  assert.ok(members.includes('crates/toolkit') && members.length >= 4, 'the member list must parse');
  const escape = text => text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const mkdir = builder.match(/mkdir -p ((?:[\w./-]+ )*[\w./-]+)/);
  assert.ok(mkdir && mkdir.index < warm, 'the builder creates the member source directories');
  const created = mkdir[1].split(' ');
  for (const member of ['', ...members]) {
    const prefix = member ? `${member}/` : '';
    const copy = member
      ? new RegExp(`^COPY ${escape(prefix)}Cargo\\.toml ${escape(prefix)}Cargo\\.toml$`, 'm')
      : /^COPY Cargo\.toml Cargo\.lock \.\/$/m;
    const copied = builder.search(copy);
    assert.ok(copied >= 0 && copied < warm, `${prefix}Cargo.toml must be copied before the dependency build`);
    const targets = ['src/main.rs', 'src/lib.rs'].filter(target => existsSync(path.join(root, prefix + target)));
    assert.ok(targets.length > 0, `${member || 'the root package'} must have a library or binary target`);
    assert.ok(created.includes(`${prefix}src`), `${prefix}src must be created before its stubs`);
    for (const target of targets) {
      const stub = builder.search(new RegExp(`> ${escape(prefix + target)}(?=\\s|$)`));
      assert.ok(stub > mkdir.index && stub < warm, `${prefix}${target} needs a stub before the dependency build`);
    }
  }
});

test('context excludes local-only descendants while retaining production inputs', () => {
  for (const rule of [
    '!crates/ci/src/apt_mirror/',
    'crates/ci/src/apt_mirror/**',
    '!crates/ci/src/apt_mirror/*.rs',
    '!crates/ci/src/automation/',
    'crates/ci/src/automation/**',
    '!crates/ci/src/automation/*.rs',
    '!crates/release/src/ops_artifact/maintain_runtime/',
    'crates/release/src/ops_artifact/maintain_runtime/**',
    '!crates/release/src/ops_artifact/maintain_runtime/*.rs',
    '!crates/release/src/ops_artifact/export/',
    'crates/release/src/ops_artifact/export/**',
    '!crates/release/src/ops_artifact/export/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/events/',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/events/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/events/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/proxy-legacy.Caddyfile',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/*.rs',
    '!ops/src/release/attestation/stage/reentry/',
    'ops/src/release/attestation/stage/reentry/**',
    '!ops/src/release/attestation/stage/reentry/*.rs',
    '!crates/',
    'crates/**',
    '!crates/toolkit/',
    'crates/toolkit/**',
    '!crates/toolkit/Cargo.toml',
    '!crates/toolkit/src/',
    'crates/toolkit/src/**',
    '!crates/toolkit/src/*.rs',
    '!crates/toolkit/src/test_env/',
    'crates/toolkit/src/test_env/**',
    '!crates/toolkit/src/test_env/*.rs',
    '!crates/ci/',
    'crates/ci/**',
    '!crates/ci/Cargo.toml',
    '!crates/ci/src/',
    'crates/ci/src/**',
    '!crates/ci/src/*.rs',
    '!crates/ci/src/automation/pins/',
    'crates/ci/src/automation/pins/**',
    '!crates/ci/src/automation/pins/*.rs',
    '!crates/release/',
    'crates/release/**',
    '!crates/release/Cargo.toml',
    '!crates/release/src/',
    'crates/release/src/**',
    '!crates/release/src/*.rs',
    '!crates/release/src/ops_artifact/',
    'crates/release/src/ops_artifact/**',
    '!crates/release/src/ops_artifact/*.rs',
    '!crates/release/src/ops_artifact/pins/',
    'crates/release/src/ops_artifact/pins/**',
    '!crates/release/src/ops_artifact/pins/*.rs',
  ]) assert.ok(ignoreRules.includes(rule), 'the fixed Rust controller needs narrow traversal');
  for (const filename of [
    '.env', 'CLAUDE.md', 'results/local/report.json',
    'ops/private.env', 'ops/src/private.env', 'ops/src/release/private.env',
    'ops/src/release/recorded_admit.json', 'ops/src/release/recorded_authorize.json',
    'ops/src/release/recorded_profile.json', 'ops/src/release/recorded_ledger.json',
    'ops/src/capacity_vps/private.env', 'ops/src/capacity_vps/retained/artifact.zip',
    'ops/src/backup/private.env', 'ops/src/backup/recorded_nightly.json',
    'ops/src/host/private.env',
    'ops/src/monitor/private.env', 'ops/src/monitor/recorded_reference.json',
    'ops/src/reboot/private.env', 'ops/src/reboot/recorded_reboot.json',
    'ops/src/turn/private.env', 'ops/src/turn/recorded_turn.json',
    'ops/src/migrate/private.env', 'ops/src/migrate/recorded_tls.json',
    'ops/src/migrate/recorded_inspect.json', 'ops/src/migrate/recorded_snapshot.json',
    'ops/src/migrate/recorded_data.json', 'ops/src/migrate/recorded_handoff.json',
    'crates/lobby-seed/private.env', 'crates/lobby-seed/fixtures/recorded_seed.json',
    'ops/tests/private.env', 'ops/src/release/archive/private.env', 'ops/README.md',
    'ops/src/release/attestation/acquire/private.env',
    'ops/src/release/attestation/acquire/retained.json',
    'ops/src/release/attestation/fetch/private.env',
    'ops/src/release/attestation/fetch/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/private.env',
    'ops/src/release/attestation/maintain_prerequisite/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/events/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/events/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/private.env',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/retained.json',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/unrelated.Caddyfile',
    'ops/src/release/attestation/receive/private.env',
    'ops/src/release/attestation/receive/retained.json',
    'ops/src/release/attestation/deploy/private.env',
    'ops/src/release/attestation/deploy/retained.json',
    'ops/src/release/attestation/stage/private.env',
    'ops/src/release/attestation/stage/reentry/private.env',
    'ops/src/release/attestation/stage/reentry/retained.json',
    'ops/src/release/attestation/restore/private.env',
    'ops/src/release/attestation/restore/retained.json',
    'ops/src/release/attestation/stage/retained.json',
    'ops/src/release/attestation/verify/private.env',
    'ops/src/release/attestation/verify/retained.json',
    'ops/unrelated-local-selection.json', 'ops/ops-build-inputs.private.json',
    'build/CLAUDE.md', 'build/private.env', 'src/CLAUDE.md', 'src/auth/private.env',
    'security/private.env', 'security/notes.json', 'security/runtime/private.env',
    'src/media/CLAUDE.md', 'src/room/notes.md', 'src/room/manager/notes.md', 'src/room/social/notes.md',
    'src/signaling/private.env',
    'migrations/private.env', 'load_tests/notes.md', 'load_tests/src/CLAUDE.md',
    'load_tests/src/metrics/private.env', 'load_tests/src/report/private.env',
    'load_tests/src/capacity/private.env', 'load_tests/src/capacity_run/private.env', 'load_tests/src/owned_process/private.env',
    'load_tests/src/bench/private.env', 'load_tests/src/bench/podman/private.env',
    'load_tests/src/bench/local/private.env',
    'load_tests/src/bench/local/receipt/private.env',
    'load_tests/src/bench/impaired/private.env',
    'load_tests/src/bench/account_room/private.env',
    'load_tests/src/bench/smoke/private.env',
    'load_tests/src/bench/smoke/reference/private.env',
    'load_tests/src/bench/smoke/reference/build/private.env',
    'load_tests/src/bench/smoke/reference/load_tests/private.env',
    'load_tests/src/bench/restart_browser/private.env',
    'load_tests/src/bench/restart_browser/reference/private.env',
    'load_tests/src/bench/restart_browser/reference/build/private.env',
    'load_tests/src/bench/restart_browser/reference/web/private.env',
    'load_tests/src/bench/restart_browser/reference/web/e2e/private.env',
    'xtask/notes.md', 'xtask/src/CLAUDE.md', 'web/CLAUDE.md', 'web/.env',
    'crates/notes.md', 'crates/private.env', 'crates/toolkit/private.env',
    'crates/toolkit/src/private.env', 'crates/toolkit/src/CLAUDE.md',
    'crates/toolkit/src/test_env/private.env',
    'crates/ci/private.env', 'crates/ci/src/private.env', 'crates/ci/src/CLAUDE.md',
    'crates/ci/tests/private.env', 'crates/ci/src/automation/pins/private.env',
    'crates/release/private.env', 'crates/release/src/private.env', 'crates/release/src/CLAUDE.md',
    'crates/release/src/bootstrap/private.env',
    'crates/release/tests/private.env', 'crates/release/src/ops_artifact/pins/private.env',
    'xtask/src/check/private.env', 'xtask/src/run_local/private.env',
    'xtask/src/ci/private.env', 'crates/ci/src/local/private.env',
    'crates/ci/src/apt_mirror/private.env',
    'crates/ci/src/apt_mirror/unreviewed.sh',
    'crates/ci/src/apt_mirror/retained/apt-mirrors.txt',
    'crates/ci/src/apt_mirror/retained/artifact.zip',
    'crates/ci/src/automation/private.env',
    'crates/ci/src/automation/retained/artifact.zip',
    'crates/ci/src/evidence/private.env',
    'crates/ci/src/retention/private.env',
    'crates/ci/src/release_http_policy/private.env',
    'crates/ci/src/release_http_policy/recorded_reference.json',
    'crates/ci/src/codeql_cache/private.env',
    'crates/ci/src/production_image/private.env',
    'crates/ci/src/install_compose/private.env',
    'crates/ci/src/audit_rust/private.env',
    'crates/ci/src/release_checks/private.env',
    'crates/ci/src/image_cache/private.env',
    'crates/ci/src/container_benchmark/private.env',
    'crates/ci/src/container_benchmark/reference/private.env',
    'crates/ci/src/container_benchmark/reference/build/private.env',
    'crates/ci/src/container_benchmark/reference/build/tests/private.env',
    'crates/ci/src/container_smoke/private.env',
    'crates/ci/src/config_validation/private.env',
    'crates/ci/src/load_generator_image/private.env',
    'crates/ci/src/public_smoke/private.env',
    'crates/ci/src/public_smoke/reference/private.env',
    'crates/ci/src/mdns/private.env',
    'crates/ci/src/mdns/reference/private.env',
    'crates/ci/src/native_dtls/private.env',
    'crates/ci/src/performance_compare/private.env',
    'crates/ci/src/performance_compare/unlisted.sh',
    'crates/release/src/ops_artifact/private.env',
    'crates/release/src/ops_artifact/maintain_runtime/private.env',
    'crates/release/src/ops_artifact/export/private.env',
    'crates/release/src/ops_artifact/deploy_frontend/private.env',
    'crates/release/src/ops_artifact/deploy_frontend/migrate/private.env',
    'crates/release/src/ops_artifact/deploy_frontend/migrate/recorded_controller.json',
    'crates/release/src/ops_artifact/deploy_frontend/migrate/recorded_vectors.json',
    'crates/release/src/ops_artifact/export/retained.json',
    'crates/release/src/ops_artifact/maintain_runtime/retained/artifact.zip',
    'crates/release/src/ops_artifact/capacity_vps/private.env',
    'crates/release/src/ops_artifact/capacity_vps/retained/ops-attestation.jsonl',
    'crates/release/src/ops_artifact/source/private.env',
    'crates/release/src/ops_artifact/registry/private.env',
    'crates/release/src/ops_artifact/verify/private.env',
    'crates/release/src/ops_artifact/verify/retained/ops-attestation.jsonl',
    'crates/release/src/ops_artifact/acquire/private.env',
    'crates/release/src/ops_artifact/acquire/artifact.zip',
    'crates/release/src/ops_artifact/acquire/retained/ops-attestation.jsonl',
    'crates/release/src/ops_artifact/hosted/private.env',
    'crates/release/src/ops_artifact/hosted/unreviewed.lock.json',
    'crates/release/src/ops_artifact/hosted/provisioned/kit/toolchain/bin/rustc',
    'web/node_modules/example/index.js', 'web/e2e/results/report.json',
    'web/scripts/CLAUDE.md', 'web/scripts/local-only.mjs',
    'web/public/private.env', 'web/public/unlisted.html', 'web/src/CLAUDE.md',
    'web/src/screens/private.env', 'web/src/screens/CLAUDE.md', 'web/src/chat/private.env', 'web/src/chat/CLAUDE.md',
    'web/src/room/private.env', 'web/src/room/CLAUDE.md', 'web/src/community/private.env', 'web/src/community/CLAUDE.md',
  ]) assert.equal(excluded(filename), true, `${filename} must stay outside the build context`);
  for (const filename of [
    'Dockerfile', '.dockerignore', 'Cargo.toml', 'Cargo.lock', 'build.rs',
    'build/pip-constraints.txt', 'build/install-openssl.sh',
    'security/exceptions.json', 'security/image-policy.json', 'security/runtime/nsswitch.conf',
    'src/main.rs', 'src/auth/mod.rs', 'src/media/mod.rs', 'src/room/mod.rs', 'src/room/manager/membership.rs',
    'src/room/social/members.rs',
    'src/signaling/mod.rs', 'migrations/001_initial.sql',
    'ops/Cargo.toml', 'ops/ops-build-inputs.lock.json', 'ops/src/main.rs', 'ops/src/release/manifest.rs',
    'ops/src/capacity_vps/protocol.rs',
    'ops/src/release/attestation/receive/channel.rs',
    'ops/src/release/attestation/receive/download.rs',
    'ops/src/release/attestation/receive/options.rs',
    'ops/src/release/attestation/deploy/execution.rs',
    'ops/src/release/attestation/deploy/replacement.rs',
    'ops/src/release/attestation/stage/files.rs',
    'ops/src/release/attestation/stage/options.rs',
    'ops/src/release/attestation/stage/process.rs',
    'ops/src/release/attestation/stage/reentry.rs',
    'ops/src/release/attestation/stage/reentry/tests.rs',
    'ops/src/release/attestation/maintain_prerequisite/guest/host.rs',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/native_report.rs',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/events.rs',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/events/proxy_upgrade.rs',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg.rs',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/tests.rs',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/proxy-legacy.Caddyfile',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/legacy_proxy.rs',
    'ops/src/release/attestation/restore/execution.rs',
    'ops/src/release/attestation/restore/options.rs',
    'ops/src/release/attestation/restore/policy.rs',
    'ops/src/release/attestation/restore/process.rs',
    'load_tests/Cargo.toml', 'load_tests/src/main.rs', 'load_tests/src/metrics/measurement.rs',
    'xtask/Cargo.toml', 'xtask/src/main.rs',
    'crates/toolkit/Cargo.toml', 'crates/toolkit/src/lib.rs',
    'crates/toolkit/src/owned_process.rs', 'crates/toolkit/src/tooling.rs',
    'crates/toolkit/src/media_ip.rs', 'crates/toolkit/src/test_env/mod.rs',
    'crates/ci/Cargo.toml', 'crates/ci/src/lib.rs', 'crates/ci/src/main.rs',
    'crates/release/Cargo.toml', 'crates/release/src/main.rs', 'crates/release/src/ops_artifact.rs',
    'crates/release/src/ops_artifact/pins.rs',
    'crates/release/src/ops_artifact/pins/ops_runtime_workflow.rs',
    'crates/ci/src/automation/pins.rs', 'crates/ci/src/automation/pins/ci_verified.rs',
    'crates/ci/src/apt_mirror.rs',
    'crates/ci/src/apt_mirror/policy.rs',
    'crates/ci/src/apt_mirror/files.rs',
    'crates/ci/src/apt_mirror/tests.rs',
    'crates/release/src/ops_artifact/capacity_vps/prepare.rs',
    'crates/release/src/ops_artifact/maintain_runtime/execute.rs',
    'crates/release/src/ops_artifact/maintain_runtime/host_baseline.rs',
    'crates/release/src/ops_artifact/maintain_runtime/qmp.rs',
    'crates/release/src/ops_artifact/export/mod.rs',
    'crates/release/src/ops_artifact/export/lifecycle.rs',
    'web/package.json', 'web/package-lock.json', 'web/tsconfig.json',
    'web/tsconfig.app.json', 'web/tsconfig.tools.json', 'web/vite.config.ts',
    'web/index.html', 'web/bundle-budget.json', 'web/scripts/check-bundle.mjs',
    'web/scripts/mediasoup-runtime.mjs',
    'web/public/help.html', 'web/public/help.css', ...pwaFiles, ...iconFiles,
    'web/src/main.ts', 'web/src/style.css', 'web/src/screens/room.ts', 'web/src/chat/composer.ts',
    'web/src/room/client.ts', 'web/src/community/controller.ts',
    'vendor/mediasoup-sys-0.19.0/subprojects/packagefiles/abseil-cpp/meson.build',
  ]) assert.equal(excluded(filename), false, `${filename} must remain available to the build`);
});


test('ops workspace inputs remain narrow and no ops binary enters the production image', () => {
  const opsRules = ignoreRules.filter(rule => rule.replace(/^!/, '').startsWith('ops'));
  assert.deepEqual(opsRules.toSorted(), [
    '!ops/', 'ops/**', '!ops/Cargo.toml', '!ops/ops-build-inputs.lock.json',
    '!ops/src/', 'ops/src/**', '!ops/src/*.rs',
    '!ops/src/capacity_vps/', 'ops/src/capacity_vps/**', '!ops/src/capacity_vps/*.rs',
    '!ops/src/backup/', 'ops/src/backup/**', '!ops/src/backup/*.rs',
    '!ops/src/host/', 'ops/src/host/**', '!ops/src/host/*.rs',
    '!ops/src/monitor/', 'ops/src/monitor/**', '!ops/src/monitor/*.rs',
    '!ops/src/reboot/', 'ops/src/reboot/**', '!ops/src/reboot/*.rs',
    '!ops/src/turn/', 'ops/src/turn/**', '!ops/src/turn/*.rs',
    '!ops/src/migrate/', 'ops/src/migrate/**', '!ops/src/migrate/*.rs',
    '!ops/src/release/', 'ops/src/release/**', '!ops/src/release/*.rs',
    '!ops/src/release/archive/', 'ops/src/release/archive/**', '!ops/src/release/archive/*.rs',
    '!ops/src/release/attestation/',
    'ops/src/release/attestation/**',
    '!ops/src/release/attestation/*.rs',
    '!ops/src/release/attestation/acquire/',
    'ops/src/release/attestation/acquire/**',
    '!ops/src/release/attestation/acquire/*.rs',
    '!ops/src/release/attestation/deploy/',
    'ops/src/release/attestation/deploy/**',
    '!ops/src/release/attestation/deploy/*.rs',
    '!ops/src/release/attestation/fetch/',
    'ops/src/release/attestation/fetch/**',
    '!ops/src/release/attestation/fetch/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/',
    'ops/src/release/attestation/maintain_prerequisite/**',
    '!ops/src/release/attestation/maintain_prerequisite/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/',
    'ops/src/release/attestation/maintain_prerequisite/guest/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/',
    'ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/deploy/injection_client/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/cold_pg/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/events/',
    'ops/src/release/attestation/maintain_prerequisite/guest/host/events/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/host/events/*.rs',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/proxy-legacy.Caddyfile',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/',
    'ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/**',
    '!ops/src/release/attestation/maintain_prerequisite/guest/effects/policy/*.rs',
    '!ops/src/release/attestation/receive/',
    'ops/src/release/attestation/receive/**',
    '!ops/src/release/attestation/receive/*.rs',
    '!ops/src/release/attestation/stage/',
    'ops/src/release/attestation/stage/**',
    '!ops/src/release/attestation/stage/*.rs',
    '!ops/src/release/attestation/stage/reentry/',
    'ops/src/release/attestation/stage/reentry/**',
    '!ops/src/release/attestation/stage/reentry/*.rs',
    '!ops/src/release/attestation/restore/',
    'ops/src/release/attestation/restore/**',
    '!ops/src/release/attestation/restore/*.rs',
    '!ops/src/release/attestation/verify/',
    'ops/src/release/attestation/verify/**',
    '!ops/src/release/attestation/verify/*.rs',
    '!ops/ansible/', 'ops/ansible/**', '!ops/ansible/files/', 'ops/ansible/files/**',
    '!ops/ansible/files/bounded_process.py', '!ops/ansible/files/release_json.py',
  ].toSorted());
  assert.match(dockerfile, /^COPY ops\/Cargo\.toml ops\/Cargo\.toml$/m);
  assert.match(dockerfile, /^COPY ops\/src \.\/ops\/src$/m);
  assert.match(dockerfile,
    /^COPY ops\/ops-build-inputs\.lock\.json \.\/ops\/ops-build-inputs\.lock\.json$/m);
  assert.match(dockerfile, /> ops\/src\/main\.rs/);
  assert.match(dockerfile, /printf '' > ops\/src\/lib\.rs/,
    'The controller path dependency needs the Ops library target during dependency warming');
  assert.match(dockerfile, /printf '' > load_tests\/src\/lib\.rs/,
    'The Ops capacity dependency needs the load-test library target during dependency warming');
  const runtime = dockerfile.slice(dockerfile.indexOf(' AS runtime-base'));
  assert.doesNotMatch(runtime,
    /^COPY .*ops\/(?:src|ops-build-inputs\.lock\.json)|^COPY .*simplestchat-ops/m);
  assert.doesNotMatch(dockerfile, /cargo[^\n]*build[^\n]*(?:-p|--bin) simplestchat-ops/);
});

// These retained references are compile/runtime inputs for inert policy tests;
// arbitrary adjacent files remain outside the image context.
test('standalone container benchmark admits only its reviewed reference closure', () => {
  for (const filename of [
    'crates/ci/src/container_benchmark/oracle.mjs',
    'crates/ci/src/container_benchmark/reference/build/benchmark-container.sh',
    'crates/ci/src/container_benchmark/reference/build/cleanup-container-benchmark.sh',
    'crates/ci/src/container_benchmark/reference/build/tests/container-benchmark.test.mjs',
  ]) assert.equal(excluded(filename), false, filename);
  assert.equal(excluded('crates/ci/src/container_benchmark/reference/build/other.sh'), true);
  assert.equal(excluded('crates/ci/src/container_benchmark/reference/build/tests/other.mjs'), true);
});


