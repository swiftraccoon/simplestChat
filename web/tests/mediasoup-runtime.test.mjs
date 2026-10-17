import assert from 'node:assert/strict';
import { readFileSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import test from 'node:test';
import {
  checkedH264Import,
  checkedVersion,
  mediasoupRuntime,
} from '../scripts/mediasoup-runtime.mjs';

const base = new URL('../node_modules/mediasoup-client/', import.meta.url);
const remoteSdp = realpathSync(fileURLToPath(new URL('lib/handlers/sdp/RemoteSdp.js', base)));
const metadata = JSON.parse(readFileSync(new URL('package.json', base), 'utf8'));
const source = readFileSync(remoteSdp);
const ortc = realpathSync(fileURLToPath(new URL('lib/ortc.js', base)));
const ortcSource = readFileSync(ortc);
const dependencyRequire = createRequire(ortc);
const h264 = realpathSync(dependencyRequire.resolve('h264-profile-level-id'));
const h264Metadata = JSON.parse(
  readFileSync(dependencyRequire.resolve('h264-profile-level-id/package.json'), 'utf8'),
);
const h264Source = readFileSync(h264);

test('the reviewed SDP import receives the identical public package version', () => {
  const plugin = mediasoupRuntime();
  const sourceImport = /const __1 = require\("([^"]+)"\)/.exec(source.toString())?.[1];
  assert.equal(sourceImport, '../../');
  const id = plugin.resolveId(sourceImport, remoteSdp);
  assert.equal(typeof id, 'string');
  assert.equal(plugin.load(id), `export const version = ${JSON.stringify(metadata.version)};`);
  assert.equal(checkedVersion(metadata, source), metadata.version);
});

test('the resolver does not replace another module, import, or explicit barrel request', () => {
  const plugin = mediasoupRuntime();
  for (const [specifier, importer] of [
    ['mediasoup-client', remoteSdp],
    ['../../index.js', remoteSdp],
    ['../..', remoteSdp],
    ['../../', remoteSdp + '.unreviewed'],
    ['../../', fileURLToPath(new URL('lib/Device.js', base))],
    ['../../', undefined],
  ]) {
    assert.equal(plugin.resolveId(specifier, importer), null);
  }
  assert.equal(plugin.load('unrelated-module'), null);
});

test('changed dependency version, package identity or SDP source requires new review', () => {
  for (const changed of [
    null,
    {},
    { ...metadata, name: 'other' },
    { ...metadata, version: '3.23.2' },
  ]) {
    assert.throws(() => checkedVersion(changed, source), /Review the pinned mediasoup/);
  }
  assert.throws(
    () => checkedVersion(metadata, Buffer.concat([source, Buffer.from('\n')])),
    /Review the pinned mediasoup/,
  );
});

test('the H264 facade applies only to the reviewed ORTC package import', () => {
  const plugin = mediasoupRuntime();
  assert.deepEqual(
    [...ortcSource.toString().matchAll(/h264\.(\w+)\(/g)].map((match) => match[1]),
    ['isSameProfile', 'generateProfileLevelIdStringForAnswer'],
  );
  const id = plugin.resolveId('h264-profile-level-id', ortc);
  assert.equal(typeof id, 'string');
  assert.equal(
    plugin.load(id),
    `export { isSameProfile, generateProfileLevelIdStringForAnswer } from ${JSON.stringify(h264)};`,
  );
  for (const [specifier, importer] of [
    ['h264-profile-level-id/lib/index.js', ortc],
    ['h264-profile-level-id', ortc + '.unreviewed'],
    ['h264-profile-level-id', ortc + '?query'],
    ['h264-profile-level-id', remoteSdp],
    ['h264-profile-level-id', undefined],
  ]) {
    assert.equal(plugin.resolveId(specifier, importer), null);
  }
});

test('H264 package resolution and both reviewed source files fail closed on changes', () => {
  assert.doesNotThrow(() => checkedH264Import(h264Metadata, ortcSource, h264Source));
  for (const changed of [
    null,
    {},
    { ...h264Metadata, name: 'other' },
    { ...h264Metadata, version: '2.3.4' },
    { ...h264Metadata, main: 'other.js' },
    { ...h264Metadata, exports: './lib/index.js' },
    { ...h264Metadata, module: './lib/index.js' },
    { ...h264Metadata, browser: './lib/index.js' },
    { ...h264Metadata, 'jsnext:main': './lib/index.js' },
    { ...h264Metadata, jsnext: './lib/index.js' },
  ]) {
    assert.throws(
      () => checkedH264Import(changed, ortcSource, h264Source),
      /Review the pinned mediasoup ORTC\/H264/,
    );
  }
  for (const [changedOrtc, changedH264] of [
    [Buffer.concat([ortcSource, Buffer.from('\n')]), h264Source],
    [ortcSource, Buffer.concat([h264Source, Buffer.from('\n')])],
  ]) {
    assert.throws(
      () => checkedH264Import(h264Metadata, changedOrtc, changedH264),
      /Review the pinned mediasoup ORTC\/H264/,
    );
  }
});

test('the real facade preserves H264 function identity, negotiation results and errors', async () => {
  const plugin = mediasoupRuntime();
  const facadeSource = plugin.load(plugin.resolveId('h264-profile-level-id', ortc));
  // Node's data: loader needs a file URL for the same real entry used by Vite.
  const facade = await import(
    'data:text/javascript,' +
      encodeURIComponent(
        facadeSource.replace(JSON.stringify(h264), JSON.stringify(pathToFileURL(h264).href)),
      )
  );
  const original = dependencyRequire('h264-profile-level-id');
  assert.deepEqual(Object.keys(facade).sort(), [
    'generateProfileLevelIdStringForAnswer',
    'isSameProfile',
  ]);
  assert.equal(facade.isSameProfile, original.isSameProfile);
  assert.equal(
    facade.generateProfileLevelIdStringForAnswer,
    original.generateProfileLevelIdStringForAnswer,
  );

  function answer(api, local, remote) {
    try {
      return api.generateProfileLevelIdStringForAnswer(local, remote);
    } catch (error) {
      return { name: error.name, message: error.message };
    }
  }
  for (const [local, remote, sameProfile, expected] of [
    [{}, {}, true, undefined],
    [{ 'profile-level-id': '42e01f' }, { 'profile-level-id': '42e015' }, true, '42e015'],
    [{ 'profile-level-id': '42f00b' }, { 'profile-level-id': '42e00b' }, true, '42f00b'],
    [
      { 'profile-level-id': '42e01f', 'level-asymmetry-allowed': 1 },
      { 'profile-level-id': '42e015', 'level-asymmetry-allowed': 1 },
      true,
      '42e01f',
    ],
    [
      { 'profile-level-id': 'bad' },
      { 'profile-level-id': '42e01f' },
      false,
      { name: 'TypeError', message: 'invalid local_profile_level_id' },
    ],
    [
      { 'profile-level-id': '42e01f' },
      { 'profile-level-id': 'bad' },
      false,
      { name: 'TypeError', message: 'invalid remote_profile_level_id' },
    ],
    [
      { 'profile-level-id': '42e01f' },
      { 'profile-level-id': '64001f' },
      false,
      { name: 'TypeError', message: 'H264 Profile mismatch' },
    ],
  ]) {
    assert.equal(facade.isSameProfile(local, remote), sameProfile);
    assert.equal(facade.isSameProfile(local, remote), original.isSameProfile(local, remote));
    assert.deepEqual(answer(facade, local, remote), expected);
    assert.deepEqual(answer(facade, local, remote), answer(original, local, remote));
  }
});
