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
  const webRules = ignoreRules.filter(rule => rule.startsWith('!web'));
  assert.deepEqual(webRules.toSorted(), [
    '!web/', '!web/package.json', '!web/package-lock.json',
    '!web/tsconfig.json', '!web/tsconfig.app.json', '!web/tsconfig.tools.json',
    '!web/vite.config.ts', '!web/index.html', '!web/bundle-budget.json',
    '!web/scripts/', '!web/scripts/check-bundle.mjs',
    '!web/public/', '!web/public/help.html', '!web/public/help.css',
    '!web/src/', '!web/src/*.ts', '!web/src/*.css',
  ].toSorted());
  assert.ok(ignoreRules.every(rule => rule === '**' || rule.startsWith('!')),
    'Later exclusions must be accounted for by the input-closure guard');
});
