import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const VERSION = '3.24.4';
const REMOTE_SDP_SHA256 = '556c727892cc42504a41a69bfb8ed7383d66eaea5ea87ee5072d0aea06f71a9e';
const VERSION_MODULE = '\0simplestchat-mediasoup-version';

/**
 * RemoteSdp uses its package barrel only for the SDP origin's version string.
 * Bind that reviewed use to exact installed bytes before replacing its one import.
 * @param {unknown} metadata
 * @param {Buffer} source
 * @returns {string}
 */
export function checkedVersion(metadata, source) {
  if (
    !metadata ||
    typeof metadata !== 'object' ||
    !('name' in metadata) ||
    metadata.name !== 'mediasoup-client' ||
    !('version' in metadata) ||
    metadata.version !== VERSION ||
    createHash('sha256').update(source).digest('hex') !== REMOTE_SDP_SHA256
  ) {
    throw new Error('Review the pinned mediasoup Device/RemoteSdp bundle contract before updating');
  }
  return metadata.version;
}

/**
 * Keep the pinned Device graph free of public test exports without replacing
 * any transport implementation or changing another module's resolution.
 * @returns {import('vite').Plugin}
 */
export function mediasoupRuntime() {
  const base = new URL('../node_modules/mediasoup-client/', import.meta.url);
  const remoteSdp = realpathSync(fileURLToPath(new URL('lib/handlers/sdp/RemoteSdp.js', base)));
  const version = checkedVersion(
    /** @type {unknown} */ (JSON.parse(readFileSync(new URL('package.json', base), 'utf8'))),
    readFileSync(remoteSdp),
  );
  return {
    name: 'simplestchat-mediasoup-runtime',
    enforce: 'pre',
    resolveId(source, importer) {
      return source === '../../' && importer === remoteSdp ? VERSION_MODULE : null;
    },
    load(id) {
      return id === VERSION_MODULE ? `export const version = ${JSON.stringify(version)};` : null;
    },
  };
}
