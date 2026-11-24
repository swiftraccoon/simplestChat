// Replays what the retired Python programs of the native installers answered:
// the AWS-LC receipt check, installation parent, download and receipt writer,
// and the archive extraction both installers ran. Each case is rebuilt from its
// recorded description, which is bound by the SHA-256 of its canonical JSON.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { chmod, lstat, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

const root = fileURLToPath(new URL('../../', import.meta.url));
const awsLc = join(root, 'build/install-aws-lc.sh');
const openssl = join(root, 'build/install-openssl.sh');
const recording = JSON.parse(await readFile(new URL('./native-installers-recorded.json', import.meta.url), 'utf8'));
const sha256 = data => createHash('sha256').update(data).digest('hex');
const generator = "#!/bin/sh\necho 'bindgen 0.73.2'\n";

function canonical(value) {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value && typeof value === 'object') {
    const fields = Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`);
    return `{${fields.join(',')}}`;
  }
  return JSON.stringify(value);
}

function cases(section) {
  const all = recording[section];
  assert.ok(all.length > 0, section);
  for (const each of all) assert.equal(sha256(canonical(each.description)), each.sha256, each.description.name);
  return all;
}

const bytes = value => (value && typeof value === 'object' ? Buffer.from(value.hex, 'hex') : Buffer.from(value, 'utf8'));

async function place(path, value) {
  await mkdir(dirname(path), { recursive: true });
  if (value && typeof value === 'object' && 'link' in value) await symlink(value.link, path);
  else if (value && typeof value === 'object' && value.directory) await mkdir(path);
  else await writeFile(path, bytes(value));
}

async function temporary(t) {
  await mkdir(join(root, 'target'), { recursive: true });
  const directory = await mkdtemp(join(root, 'target', 'native-installer-replay-'));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

// Run an installer under the umask CI and the image builds use, or another.
function install(script, prefix, env, umask = '022') {
  return spawnSync('/bin/sh', ['-c', `umask ${umask} && exec /bin/sh "$0" "$1"`, script, prefix], {
    cwd: root, encoding: 'utf8', timeout: 60_000, env: { ...process.env, ...env },
  });
}

// Fake curl, cargo, cmake, go and an archive's Configure, driven by a settings file.
const DISPATCHER = String.raw`
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const settings = JSON.parse(fs.readFileSync(process.env.INSTALLER_FAKE_SETTINGS, 'utf8'));
const [tool, ...args] = process.argv.slice(2);
const after = name => args[args.indexOf(name) + 1];
const bytes = value => (value && typeof value === 'object' ? Buffer.from(value.hex, 'hex') : Buffer.from(value, 'utf8'));
function place(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  if (value && typeof value === 'object' && 'link' in value) fs.symlinkSync(value.link, file);
  else fs.writeFileSync(file, bytes(value));
}
function snapshot(directory, skipped) {
  const out = [];
  const walk = (current) => {
    for (const name of fs.readdirSync(current).sort()) {
      const file = path.join(current, name);
      const rel = path.relative(directory, file);
      if (current === directory && skipped.includes(name)) continue;
      const info = fs.lstatSync(file);
      const entry = { path: rel, mtime: Math.floor(info.mtimeMs / 1000) };
      if (info.isDirectory()) Object.assign(entry, { kind: 'd', mode: info.mode & 0o7777 });
      else if (info.isFile()) {
        const data = fs.readFileSync(file);
        Object.assign(entry, { kind: 'f', mode: info.mode & 0o7777, size: data.length,
          sha256: crypto.createHash('sha256').update(data).digest('hex') });
      } else if (info.isSymbolicLink()) Object.assign(entry, { kind: 'l', target: fs.readlinkSync(file) });
      else Object.assign(entry, { kind: 'o', mode: info.mode & 0o7777 });
      out.push(entry);
      if (info.isDirectory()) walk(file);
    }
  };
  walk(directory);
  fs.writeFileSync(settings.snapshot, JSON.stringify(out));
  process.exit(77);
}
if (tool === 'curl') {
  const url = args.find(arg => arg.startsWith('https://'));
  const answer = settings.downloads.find(each => url.includes(each.match));
  if (!answer || answer.exit) process.exit(answer ? answer.exit : 22);
  const output = after('--output');
  if (answer.zeros !== undefined) { fs.writeFileSync(output, ''); fs.truncateSync(output, answer.zeros); }
  else fs.copyFileSync(answer.file, output);
} else if (tool === 'cargo') {
  if (settings.snapshot) snapshot(path.dirname(after('--path')), settings.skipped);
  const bin = path.join(after('--root'), 'bin');
  fs.mkdirSync(bin, { recursive: true });
  fs.writeFileSync(path.join(bin, 'bindgen'), settings.generator, { mode: 0o755 });
} else if (tool === 'configure') {
  snapshot(path.dirname(process.cwd()), settings.skipped);
} else if (tool === 'cmake') {
  if (args[0] === '-S') {
    const build = after('-B');
    fs.mkdirSync(build, { recursive: true });
    if (path.basename(build) === 'prefixed') {
      fs.writeFileSync(path.join(build, 'CMakeCache.txt'), bytes(settings.cache));
      const prefix = args.find(arg => arg.startsWith('-DCMAKE_INSTALL_PREFIX=')).split('=')[1];
      fs.writeFileSync(path.join(build, 'install-prefix'), prefix);
    }
  } else if (args[0] === '--build' && path.basename(args[1]) === 'unprefixed') {
    place(path.join(args[1], 'crypto/libcrypto.a'), 'unprefixed library\n');
  } else if (args[0] === '--install') {
    const prefix = fs.readFileSync(path.join(args[1], 'install-prefix'), 'utf8');
    for (const [name, value] of Object.entries(settings.staged)) place(path.join(prefix, name), value);
  }
} else if (tool === 'go') {
  place(after('-out'), settings.symbols);
} else process.exit(93);
`;

async function fakes(directory, settings) {
  const bin = join(directory, 'bin');
  await mkdir(bin, { recursive: true });
  const dispatcher = join(directory, 'tool.cjs');
  const file = join(directory, 'settings.json');
  await writeFile(dispatcher, DISPATCHER);
  await writeFile(file, JSON.stringify({ generator, skipped: [], ...settings }));
  const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
  for (const tool of ['curl', 'cargo', 'cmake', 'go', 'configure']) {
    await writeFile(join(bin, tool), `#!/bin/sh\nexec ${quote(process.execPath)} ${quote(dispatcher)} ${tool} "$@"\n`,
      { mode: 0o755 });
  }
  return {
    INSTALLER_FAKE_SETTINGS: file, INSTALLER_FAKE: join(bin, 'configure'), PATH: `${bin}:${process.env.PATH}`,
  };
}

// A copy of an installer whose pinned digests are the fixture archives'.
async function pinned(directory, script, pins) {
  let source = await readFile(script, 'utf8');
  for (const [name, digest] of Object.entries(pins)) {
    const pattern = new RegExp(`^${name}='[0-9a-f]{64}'$`, 'gm');
    assert.equal(source.match(pattern)?.length, 1, name);
    source = source.replace(pattern, `${name}='${digest}'`);
  }
  const copy = join(directory, script.endsWith('install-aws-lc.sh') ? 'install-aws-lc.sh' : 'install-openssl.sh');
  await writeFile(copy, source, { mode: 0o755 });
  return copy;
}

// A gzip-compressed ustar archive of described members.
function archive(members, top) {
  const blocks = [];
  const kinds = { file: '0', hardlink: '1', symlink: '2', character: '3', block: '4', directory: '5', fifo: '6' };
  for (const member of members) {
    let name = member.name.replaceAll('@TOP@', top);
    if (member.type === 'directory' && !name.endsWith('/')) name += '/';
    const data = member.type === 'file' ? bytes(member.content) : Buffer.alloc(0);
    const header = Buffer.alloc(512);
    const field = (offset, length, text) => header.write(text, offset, length, 'utf8');
    const octal = (offset, length, value) => field(offset, length, `${value.toString(8).padStart(length - 1, '0')}\0`);
    assert.ok(Buffer.byteLength(name) <= 100, name);
    field(0, 100, name);
    octal(100, 8, Number.parseInt(member.mode, 8));
    octal(108, 8, 0);
    octal(116, 8, 0);
    octal(124, 12, data.length);
    octal(136, 12, 1_000_000_000);
    header.fill(' ', 148, 156);
    field(156, 1, kinds[member.type]);
    if (member.target !== undefined) field(157, 100, member.target.replaceAll('@TOP@', top));
    field(257, 8, 'ustar\x0000');
    octal(329, 8, 0);
    octal(337, 8, 0);
    let sum = 0;
    for (const byte of header) sum += byte;
    field(148, 8, `${sum.toString(8).padStart(6, '0')}\0 `);
    blocks.push(header, data, Buffer.alloc((512 - (data.length % 512)) % 512));
  }
  blocks.push(Buffer.alloc(1024));
  return gzipSync(Buffer.concat(blocks));
}

const crate = archive([
  { name: 'bindgen-cli-0.73.2', type: 'directory', mode: '0755' },
  { name: 'bindgen-cli-0.73.2/Cargo.toml', type: 'file', mode: '0644', content: '[package]\n' },
], '');
const source = archive([
  { name: '@TOP@', type: 'directory', mode: '0775' },
  { name: '@TOP@/README', type: 'file', mode: '0664', content: 'fixture source\n' },
], 'aws-lc-5.11.0');

async function writeArchives(directory, sourceArchive) {
  const files = { source: join(directory, 'source.tar.gz'), crate: join(directory, 'crate.tar.gz') };
  await writeFile(files.source, sourceArchive);
  await writeFile(files.crate, crate);
  return files;
}

// Cases the replacement refuses although the program accepted them, each
// with the reason it is stricter.
const STRICTER_RECEIPTS = [
  'this installer, compact JSON', // only the installer's own receipt text is complete
  'this installer, keys reordered',
  'this installer, no final newline',
  'this installer, trailing blank line',
  'duplicate installer digests, the last this one',
  'a recorded file is a link to the same bytes', // recorded files must be regular files
  'the receipt is a link to this installer\'s receipt',
  'no recorded files', // exactly the eight installed files
  'two recorded files',
  'a ninth recorded file',
  'an absolute recorded name outside the prefix',
  'a recorded option differs from the cache', // the options must be the cache's own
  'the cache records another option value',
  'a generator digest that is no digest',
];
const STRICTER_ARCHIVES = [
  'a read-only file 0444', // ordinary modes only, so the tree is the filter's
  'a file executable by others only 0654',
  'a directory 0700',
  'a set-user-id file', // the filter cleared these bits
  'a set-group-id file',
  'a sticky directory',
  'a link inside', // no links at all
  'a hard link inside',
  'an absolute name', // the filter stripped the slash
  'a parent component that stays inside',
];
const STRICTER_WRITER = [
  'an option without a type and a colon in its value', // values JSON writes unescaped
  'an option value with a space',
  'an option value with a quotation mark',
  'an option value with an equals sign',
  'carriage-return line ends', // the cache must be plain ASCII text
  'a non-ASCII comment',
  'a form feed in a comment',
  'a NUL byte in a comment',
  'a header that is a link', // recorded files must be regular files
];

function stricter(all, names, accepted) {
  for (const name of names) {
    const found = all.find(each => each.description.name === name);
    assert.ok(found && accepted(found), name);
  }
  return new Set(names);
}

test('an installation is retained exactly when the program found it complete, or refused more strictly', async t => {
  const all = cases('receipts');
  const harder = stricter(all, STRICTER_RECEIPTS, each => each.complete);
  const installer = sha256(await readFile(awsLc));
  let retained = 0;
  for (const each of all) {
    const directory = await temporary(t);
    const { description } = each;
    for (const [name, value] of Object.entries(description.outside)) await place(join(directory, name), value);
    const prefix = join(directory, 'prefix');
    for (const [name, value] of Object.entries(description.files)) await place(join(prefix, name), value);
    const fill = data => Buffer.from(data.toString('latin1')
      .replaceAll('@INSTALLER_UPPER@', installer.toUpperCase())
      .replaceAll('@INSTALLER@', installer)
      .replaceAll('@ROOT@', directory), 'latin1');
    const receipt = join(prefix, 'share/simplestchat/build.json');
    if (description.receipt && typeof description.receipt === 'object' && 'link' in description.receipt) {
      await place(receipt, description.receipt);
      const target = join(prefix, 'share/simplestchat', description.receipt.link);
      await writeFile(target, fill(await readFile(target)));
    } else {
      await mkdir(dirname(receipt), { recursive: true });
      await writeFile(receipt, fill(bytes(description.receipt)));
    }
    const env = await fakes(directory, { downloads: [] });
    const result = install(awsLc, prefix, env);
    const complete = each.complete && !harder.has(description.name);
    if (complete) {
      retained += 1;
      assert.equal(result.status, 0, `${description.name}: ${result.stderr}`);
      assert.match(result.stderr, /already installed by this installer/, description.name);
    } else {
      assert.equal(result.status, 22, `${description.name}: ${result.stderr}`);
      assert.match(result.stderr, /Replacing the AWS-LC installation/, description.name);
    }
  }
  assert.equal(retained, 1);
});

test('the installation parent is refused exactly as the program refused it', async t => {
  for (const each of cases('parents')) {
    const { description } = each;
    if (description.kind === 'root-owned' && process.getuid() === 0) continue;
    const directory = await temporary(t);
    let parent = '/usr';
    if (description.kind !== 'root-owned') {
      const real = join(directory, 'real');
      await mkdir(real);
      await chmod(real, Number.parseInt(description.mode, 8));
      parent = real;
      if (description.kind === 'link') {
        parent = join(directory, 'parent');
        await symlink(real, parent);
      }
    }
    const env = await fakes(directory, { downloads: [] });
    const result = install(awsLc, join(parent, `prefix-${process.pid}`), env);
    if (description.kind !== 'root-owned') await chmod(join(directory, 'real'), 0o700);
    const words = /AWS-LC installation parent must be an owned directory, not writable by others/;
    const label = JSON.stringify(description);
    if (each.ending === 'refused') {
      assert.equal(result.status, 1, `${label}: ${result.stderr}`);
      assert.match(result.stderr, words, label);
    } else {
      assert.equal(each.ending, 'accepted');
      assert.doesNotMatch(result.stderr, words, label);
      // A parent its owner cannot write admits no private build directory.
      const writable = Number.parseInt(description.mode, 8) & 0o200;
      assert.equal(result.status, writable ? 22 : 1, `${label}: ${result.stderr}`);
    }
  }
});

test('a download is refused by size, then by digest, exactly as the program refused it', async t => {
  for (const each of cases('downloads')) {
    const { description } = each;
    const directory = await temporary(t);
    const fixture = join(directory, 'download');
    if (description.content !== undefined) await writeFile(fixture, bytes(description.content));
    const answer = description.zeros === undefined ? { file: fixture } : { zeros: description.zeros };
    const env = await fakes(directory, { downloads: [{ match: 'github.com/aws/aws-lc', ...answer }] });
    const copy = await pinned(directory, awsLc, { awslc_sha256: description.pin.toLowerCase() });
    // An upper-case pin cannot be spelled in the installer; its case is the digest's.
    if (description.pin !== description.pin.toLowerCase()) {
      const text = (await readFile(copy, 'utf8')).replace(`awslc_sha256='${description.pin.toLowerCase()}'`,
        `awslc_sha256='${description.pin}'`);
      await writeFile(copy, text);
    }
    const result = install(copy, join(directory, 'prefix'), env);
    if (each.ending === 'accepted') {
      assert.equal(result.status, 22, `${description.name}: ${result.stderr}`);
      assert.doesNotMatch(result.stderr, /size limit|SHA-256 mismatch/, description.name);
    } else {
      assert.equal(result.status, 1, `${description.name}: ${result.stderr}`);
      assert.ok(result.stderr.includes(each.words), description.name);
    }
  }
});

function expectedTree(recorded, top) {
  return recorded.map(entry => {
    const value = { path: entry.path.replace('@TOP@', top), kind: entry.kind };
    if (entry.kind !== 'l') value.mode = Number.parseInt(entry.mode, 8);
    if (entry.mtime !== null) value.mtime = entry.mtime;
    if (entry.kind === 'f') Object.assign(value, { size: entry.size, sha256: entry.sha256 });
    if (entry.kind === 'l') value.target = entry.target;
    return value;
  }).sort((left, right) => left.path.localeCompare(right.path));
}

function observedTree(observed, recorded, top) {
  const timed = new Set(recorded.filter(entry => entry.mtime !== null)
    .map(entry => entry.path.replace('@TOP@', top)));
  return observed.map(entry => {
    const value = { ...entry };
    if (!timed.has(entry.path)) delete value.mtime;
    return value;
  }).sort((left, right) => left.path.localeCompare(right.path));
}

// Each installer, its source directory and the build files beside it.
const UNPACKERS = [
  [awsLc, 'aws-lc-5.11.0', ['aws-lc.tar.gz', 'bindgen-cli.crate', 'archive-members', 'archive-names', 'bindgen-cli-0.73.2']],
  [openssl, 'openssl-4.1.0', ['openssl-4.1.0.tar.gz', 'archive-members', 'archive-names']],
];

// Run an installer copy pinned to a case archive until its tree is snapshotted.
async function unpackCase(t, installer, top, skipped, members, umask) {
  const directory = await temporary(t);
  const data = archive(members, top);
  const files = await writeArchives(directory, data);
  const snapshot = join(directory, 'snapshot.json');
  const env = await fakes(directory, {
    snapshot, skipped,
    downloads: [{ match: 'github.com/aws/aws-lc', file: files.source }, { match: 'bindgen-cli', file: files.crate },
      { match: 'github.com/openssl', file: files.source }],
  });
  const copy = installer === awsLc
    ? await pinned(directory, awsLc, { awslc_sha256: sha256(data), bindgen_sha256: sha256(crate) })
    : await pinned(directory, openssl, { openssl_sha256: sha256(data) });
  return { result: install(copy, join(directory, 'prefix'), env, umask), snapshot };
}

for (const [installer, top, skipped] of UNPACKERS) {
  test(`${top} archives unpack to the program's tree or are refused, at least as strictly`, async t => {
    const all = cases('archives');
    const harder = stricter(all, STRICTER_ARCHIVES, each => each.ending === 'accepted');
    let unpacked = 0;
    for (const each of all) {
      const { description } = each;
      const { result, snapshot } = await unpackCase(t, installer, top, skipped, description.members, '022');
      const label = description.name;
      if (each.ending === 'accepted' && !harder.has(label)) {
        if (installer === openssl && description.members.length === 0) {
          // No source directory to configure in: the installer stops there.
          assert.notEqual(result.status, 0, label);
          continue;
        }
        unpacked += 1;
        assert.equal(result.status, 77, `${label}: ${result.stderr}`);
        const observed = JSON.parse(await readFile(snapshot, 'utf8'));
        assert.deepEqual(observedTree(observed, each.tree, top), expectedTree(each.tree, top), label);
      } else {
        assert.equal(result.status, 1, `${label}: ${result.stderr}`);
        assert.match(result.stderr, /archive|build input holds|build input names|outside its directory/, label);
      }
    }
    // 18 recorded trees, 10 refused more strictly; the OpenSSL installer has
    // no source directory to configure in for the empty archive.
    assert.equal(unpacked, installer === awsLc ? 8 : 7);
  });
}

test('the receipt is the one the program wrote, or the build is refused at least as strictly', async t => {
  const all = cases('writer');
  const harder = stricter(all, STRICTER_WRITER, each => each.ending === 'accepted');
  let written = 0;
  for (const each of all) {
    const { description } = each;
    const directory = await temporary(t);
    const files = await writeArchives(directory, source);
    const env = await fakes(directory, {
      cache: description.cache, staged: description.staged, symbols: description.symbols,
      downloads: [{ match: 'github.com/aws/aws-lc', file: files.source }, { match: 'bindgen-cli', file: files.crate }],
    });
    const copy = await pinned(directory, awsLc, { awslc_sha256: sha256(source), bindgen_sha256: sha256(crate) });
    const prefix = join(directory, 'prefix');
    const result = install(copy, prefix, env);
    const label = description.name;
    if (each.ending === 'accepted' && !harder.has(label)) {
      written += 1;
      assert.equal(result.status, 0, `${label}: ${result.stderr}`);
      const expected = each.receipt.replace('@INSTALLER@', sha256(await readFile(copy)))
        .replace('@ARCHIVE@', sha256(source)).replace('@CRATE@', sha256(crate));
      assert.equal(await readFile(join(prefix, 'share/simplestchat/build.json'), 'utf8'), expected, label);
    } else {
      assert.notEqual(result.status, 0, `${label}: ${result.stderr}`);
      await assert.rejects(lstat(prefix), { code: 'ENOENT' }, label);
    }
  }
  // 16 recorded receipts, 9 refused more strictly.
  assert.equal(written, 7);
});

test('the receipt check finds the writer\'s own receipt complete after a real installation shape', async t => {
  // The writer and the check share one definition of the receipt: a prefix the
  // replayed writer produced is retained by the unchanged installer copy.
  const directory = await temporary(t);
  const files = await writeArchives(directory, source);
  const accepted = recording.writer.find(each => each.description.name === 'the build\'s own options');
  const env = await fakes(directory, {
    cache: accepted.description.cache, staged: accepted.description.staged, symbols: accepted.description.symbols,
    downloads: [{ match: 'github.com/aws/aws-lc', file: files.source }, { match: 'bindgen-cli', file: files.crate }],
  });
  const copy = await pinned(directory, awsLc, { awslc_sha256: sha256(source), bindgen_sha256: sha256(crate) });
  const prefix = join(directory, 'prefix');
  assert.equal(install(copy, prefix, env).status, 0);
  const again = install(copy, prefix, env);
  assert.equal(again.status, 0, again.stderr);
  assert.match(again.stderr, /already installed by this installer/);
  await writeFile(join(prefix, 'include/openssl/base.h'), 'changed header\n');
  const damaged = install(copy, prefix, env);
  assert.equal(damaged.status, 0, damaged.stderr);
  assert.match(damaged.stderr, /Replacing the AWS-LC installation/);
  assert.equal(await readFile(join(prefix, 'include/openssl/base.h'), 'utf8'), 'recorded header\n');
});

test('unpacked modes are the archive\'s under umask 022, whatever the caller\'s umask', async t => {
  // The filter wrote file modes without the caller's umask; the installers
  // extract under umask 022 for the same files and directories.
  const plain = recording.archives.find(each => each.description.name === 'plain files and directories');
  for (const [installer, top, skipped] of UNPACKERS) {
    for (const umask of ['077', '002']) {
      const { result, snapshot } = await unpackCase(t, installer, top, skipped, plain.description.members, umask);
      assert.equal(result.status, 77, result.stderr);
      const observed = JSON.parse(await readFile(snapshot, 'utf8'));
      assert.deepEqual(observedTree(observed, plain.tree, top), expectedTree(plain.tree, top), `${top} ${umask}`);
    }
  }
});
