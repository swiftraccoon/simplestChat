import assert from 'node:assert/strict';
import { readFileSync, realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import { checkedVersion, mediasoupRuntime } from '../scripts/mediasoup-runtime.mjs';

const base = new URL('../node_modules/mediasoup-client/', import.meta.url);
const remoteSdp = realpathSync(fileURLToPath(new URL('lib/handlers/sdp/RemoteSdp.js', base)));
const metadata = JSON.parse(readFileSync(new URL('package.json', base), 'utf8'));
const source = readFileSync(remoteSdp);

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
