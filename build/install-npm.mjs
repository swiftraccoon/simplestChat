#!/usr/bin/env node
import { createHash, timingSafeEqual } from 'node:crypto';
import { execFile } from 'node:child_process';
import { constants } from 'node:fs';
import { chmod, lstat, mkdir, mkdtemp, open, readFile, readdir, readlink, rename, rm, symlink, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

export const NPM_VERSION = '12.2.0';
export const NPM_URL = `https://registry.npmjs.org/npm/-/npm-${NPM_VERSION}.tgz`;
export const NPM_INTEGRITY = 'sha512-ZsJjKpTnlmSXOLLXiU1xDCzC4Wlok4IwZmh/aw2KUuXytU7q6qMv/cUT7MoeSf95Slwuw/lRXYefGzGCspHPNQ==';
const MAX_ARCHIVE_BYTES = 16 * 1024 * 1024;
const execute = promisify(execFile);

export function verifyArchive(bytes) {
  const expected = Buffer.from(NPM_INTEGRITY.slice('sha512-'.length), 'base64');
  const actual = createHash('sha512').update(bytes).digest();
  if (!timingSafeEqual(actual, expected)) throw new Error('npm archive integrity mismatch');
}

async function downloadArchive(fetchArchive) {
  const response = await fetchArchive(NPM_URL, {
    redirect: 'error', signal: AbortSignal.timeout(60_000),
  });
  if (!response.ok || !response.body) throw new Error(`npm download failed: HTTP ${response.status}`);
  const chunks = [];
  let size = 0;
  for await (const chunk of response.body) {
    size += chunk.length;
    if (size > MAX_ARCHIVE_BYTES) throw new Error('npm archive exceeds download limit');
    chunks.push(chunk);
  }
  const bytes = Buffer.concat(chunks);
  verifyArchive(bytes);
  return bytes;
}

async function ownedDirectory(path) {
  let stat;
  try { stat = await lstat(path); } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
  if (!stat.isDirectory() || stat.uid !== process.getuid() || (stat.mode & 0o022)) {
    throw new Error(`npm installation directory must be owned and not writable by others: ${path}`);
  }
  return true;
}

async function contents(directory, prefix = '') {
  const result = [];
  const entries = await readdir(directory, { withFileTypes: true });
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    const name = entry.name;
    const path = join(directory, name);
    const relative = prefix + name;
    if (entry.isSymbolicLink()) result.push([relative, 'link', await readlink(path)]);
    else if (entry.isDirectory()) result.push([relative, 'directory'], ...await contents(path, `${relative}/`));
    else if (entry.isFile()) {
      // Inspect and hash the opened object. A path replaced after readdir must
      // never redirect this comparison through a symlink or block on a FIFO.
      const file = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      try {
        const opened = await file.stat();
        if (!opened.isFile()) throw new Error(`Unexpected npm installation entry: ${relative}`);
        result.push([relative, opened.mode & 0o111, createHash('sha256').update(await file.readFile()).digest('hex')]);
      } finally {
        await file.close();
      }
    } else throw new Error(`Unexpected npm installation entry: ${relative}`);
  }
  return result;
}

// Build in private sibling storage. A failed preparation never replaces an
// installed tool, and an existing tool must match newly authenticated contents.
export async function installDirectory(destination, prepare) {
  const prefix = resolve(destination);
  const parent = dirname(prefix);
  if (prefix === parent) throw new Error('npm installation prefix cannot be a filesystem root');
  await mkdir(parent, { recursive: true, mode: 0o700 });
  await ownedDirectory(parent);
  await ownedDirectory(prefix);
  const staging = await mkdtemp(join(parent, '.npm-install-'));
  await chmod(staging, 0o700);
  try {
    await prepare(staging);
    if (await ownedDirectory(prefix)) {
      if (JSON.stringify(await contents(prefix)) !== JSON.stringify(await contents(staging))) {
        throw new Error('Existing npm installation differs from the authenticated archive; choose a new prefix');
      }
    } else {
      await rename(staging, prefix);
    }
    return prefix;
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}

export async function installNpm(destination, { fetchArchive = fetch } = {}) {
  return installDirectory(destination, async staging => {
    const bytes = await downloadArchive(fetchArchive);
    const archive = join(staging, 'npm.tgz');
    const packageDirectory = join(staging, 'lib', 'node_modules', 'npm');
    await writeFile(archive, bytes, { flag: 'wx', mode: 0o600 });
    await mkdir(packageDirectory, { recursive: true, mode: 0o700 });
    await execute('tar', ['-xzf', archive, '--strip-components=1', '--no-same-owner', '-C', packageDirectory], {
      timeout: 60_000, maxBuffer: 1024 * 1024,
    });
    await rm(archive);
    const metadata = JSON.parse(await readFile(join(packageDirectory, 'package.json'), 'utf8'));
    if (metadata.name !== 'npm' || metadata.version !== NPM_VERSION) throw new Error('Unexpected npm package identity');
    await mkdir(join(staging, 'bin'), { mode: 0o700 });
    for (const name of ['npm', 'npx']) {
      await chmod(join(packageDirectory, 'bin', `${name}-cli.js`), 0o755);
      await symlink(`../lib/node_modules/npm/bin/${name}-cli.js`, join(staging, 'bin', name));
    }
    const userConfig = join(staging, 'user.npmrc');
    const globalConfig = join(staging, 'global.npmrc');
    const cache = join(staging, 'version-cache');
    await writeFile(userConfig, '', { flag: 'wx', mode: 0o600 });
    await writeFile(globalConfig, '', { flag: 'wx', mode: 0o600 });
    const { stdout } = await execute(process.execPath, [join(packageDirectory, 'bin', 'npm-cli.js'), '--version'], {
      cwd: staging, timeout: 30_000, maxBuffer: 1024 * 1024,
      env: { ...process.env, NPM_CONFIG_USERCONFIG: userConfig, NPM_CONFIG_GLOBALCONFIG: globalConfig, NPM_CONFIG_CACHE: cache },
    });
    if (stdout.trim() !== NPM_VERSION) throw new Error('Installed npm version verification failed');
    await rm(userConfig);
    await rm(globalConfig);
    await rm(cache, { recursive: true, force: true });
  });
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    if (process.argv.length !== 3) throw new Error('Usage: node build/install-npm.mjs <prefix>');
    const prefix = await installNpm(process.argv[2]);
    console.log(`Verified npm ${NPM_VERSION}: ${join(prefix, 'bin')}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
