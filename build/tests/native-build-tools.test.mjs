import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const worker = 'vendor/mediasoup-sys-0.17.0/';
const read = name => readFileSync(new URL(`../../${name}`, import.meta.url), 'utf8');

test('every native Python tool is version and wheel-hash locked', () => {
  const constraints = read('build/pip-constraints.txt').split('\n')
    .filter(line => line && !line.startsWith('#')).toSorted();
  const requirements = [];
  for (const file of ['python-invoke-requirements.txt', 'python-tools-requirements.txt']) {
    const lock = read(worker + file);
    assert.match(lock, /^--require-hashes$/m);
    assert.match(lock, /^--only-binary=:all:$/m);
    for (const line of lock.replace(/\\\n\s*/g, ' ').split('\n')) {
      if (!line || line.startsWith('#') || line.startsWith('--')) continue;
      assert.match(line, /^[a-z][a-z-]*==\d[\w.]*\s+(?:--hash=sha256:[a-f0-9]{64}\s*)+$/);
      requirements.push(line.split(' ')[0]);
    }
    for (const manifest of ['Cargo.toml', 'Cargo.toml.orig', 'build.rs']) {
      assert.ok(read(worker + manifest).includes(file), `${manifest} must include ${file}`);
    }
  }
  assert.deepEqual(requirements.toSorted(), constraints);
});

test('unsupported upstream Docker tasks fail before executing commands', () => {
  const result = spawnSync('python3', ['-c', `
import ast, pathlib
path = pathlib.Path('${worker}tasks.py')
tree = ast.parse(path.read_text())
names = {'docker', 'docker_run', 'docker_alpine', 'docker_alpine_run', 'docker_386', 'docker_386_run'}
found = set()
class Context:
    def run(self, *args, **kwargs):
        raise AssertionError('unsupported task executed a command')
for node in tree.body:
    if isinstance(node, ast.FunctionDef) and node.name in names:
        node.decorator_list = []
        namespace = {}
        exec(compile(ast.Module(body=[node], type_ignores=[]), str(path), 'exec'), namespace)
        try:
            namespace[node.name](Context())
        except RuntimeError as error:
            if 'disabled' not in str(error):
                raise
        else:
            raise AssertionError('unsupported task did not fail closed')
        found.add(node.name)
if found != names:
    raise AssertionError('missing task guard')
`], { cwd: root, encoding: 'utf8', timeout: 10_000 });
  assert.equal(result.status, 0, result.stderr);
  const importer = spawnSync('bash', [worker + 'scripts/get-dep.sh', 'fuzzer-corpora'], {
    cwd: root, encoding: 'utf8', timeout: 10_000,
  });
  assert.equal(importer.status, 1);
  assert.match(importer.stderr, /disabled/);
});
