import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, readdir, rm, symlink, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { installDirectory, installNpm, NPM_URL, NPM_VERSION, verifyArchive } from '../install-npm.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));

async function temporary(t) {
  await mkdir(join(root, 'target'), { recursive: true });
  const directory = await mkdtemp(join(root, 'target', 'npm-installer-test-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

test('changed archive bytes fail before extraction or activation', async t => {
  const directory = await temporary(t);
  assert.throws(() => verifyArchive(Buffer.from('untrusted npm')), /integrity mismatch/);
  await assert.rejects(installNpm(join(directory, 'tool'), {
    fetchArchive: async (url, options) => {
      assert.equal(url, NPM_URL);
      assert.equal(options.redirect, 'error');
      assert.ok(options.signal instanceof AbortSignal);
      return new Response('untrusted npm');
    },
  }), /integrity mismatch/);
  assert.deepEqual(await readdir(directory), []);
});

test('failed download and excessive archive size clean private staging', async t => {
  const directory = await temporary(t);
  for (const [fetchArchive, error] of [
    [async () => { throw new Error('network failure'); }, /network failure/],
    [async () => new Response('', { status: 503 }), /HTTP 503/],
    [async () => new Response(new Uint8Array(16 * 1024 * 1024 + 1)), /download limit/],
  ]) {
    await assert.rejects(installNpm(join(directory, 'tool'), { fetchArchive }), error);
    assert.deepEqual(await readdir(directory), []);
  }
});

test('failed preparation preserves the existing installation', async t => {
  const directory = await temporary(t);
  const prefix = join(directory, 'tool');
  await mkdir(prefix);
  await writeFile(join(prefix, 'important'), 'retained');
  await assert.rejects(installDirectory(prefix, async staging => {
    await writeFile(join(staging, 'partial'), 'unfinished');
    throw new Error('version verification failed');
  }), /version verification failed/);
  assert.equal(await readFile(join(prefix, 'important'), 'utf8'), 'retained');
  assert.deepEqual(await readdir(directory), ['tool']);
});

test('identical authenticated contents are reusable and modified contents are refused', async t => {
  const directory = await temporary(t);
  const prefix = join(directory, 'tool');
  const prepare = async staging => {
    await writeFile(join(staging, 'npm.js'), NPM_VERSION, { mode: 0o755 });
    await symlink('npm.js', join(staging, 'npm'));
  };
  assert.equal(await installDirectory(prefix, prepare), prefix);
  assert.equal(await installDirectory(prefix, prepare), prefix);
  await writeFile(join(prefix, 'npm.js'), 'modified');
  await assert.rejects(installDirectory(prefix, prepare), /differs from the authenticated archive/);
  assert.equal(await readFile(join(prefix, 'npm.js'), 'utf8'), 'modified');
  assert.deepEqual(await readdir(directory), ['tool']);
});

test('a symlink destination fails before any download or writes through it', async t => {
  const directory = await temporary(t);
  const unrelated = join(directory, 'unrelated');
  await mkdir(unrelated);
  const prefix = join(directory, 'tool');
  await symlink(unrelated, prefix);
  await assert.rejects(installNpm(prefix, {
    fetchArchive: () => { throw new Error('must not download'); },
  }), /installation directory must be owned/);
  assert.deepEqual(await readdir(unrelated), []);
});
