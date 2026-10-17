import { createHash } from 'node:crypto';
import { readFileSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const VERSION = '3.24.4';
const REMOTE_SDP_SHA256 = '556c727892cc42504a41a69bfb8ed7383d66eaea5ea87ee5072d0aea06f71a9e';
const VERSION_MODULE = '\0simplestchat-mediasoup-version';
const ORTC_SHA256 = '4fd4650005ce07bd8f8717ddd2cd64dc529134120102b34b75dc716adc82a635';
const H264_VERSION = '2.3.3';
const H264_SHA256 = '6082edc420be755776ffe5eeadcf9de8a2c69016346d25d31362a249a67739a7';
const H264_MODULE = '\0simplestchat-mediasoup-h264';

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
 * The reviewed ORTC module calls only these two H264 exports, without observing
 * their receiver. Re-export the original functions so their validation, logging
 * and single module initialization remain intact while unused APIs can be removed.
 * @param {unknown} metadata
 * @param {Buffer} ortcSource
 * @param {Buffer} h264Source
 * @returns {void}
 */
export function checkedH264Import(metadata, ortcSource, h264Source) {
  if (
    !metadata ||
    typeof metadata !== 'object' ||
    !('name' in metadata) ||
    metadata.name !== 'h264-profile-level-id' ||
    !('version' in metadata) ||
    metadata.version !== H264_VERSION ||
    !('main' in metadata) ||
    metadata.main !== 'lib/index.js' ||
    'exports' in metadata ||
    'module' in metadata ||
    'browser' in metadata ||
    'jsnext:main' in metadata ||
    'jsnext' in metadata ||
    createHash('sha256').update(ortcSource).digest('hex') !== ORTC_SHA256 ||
    createHash('sha256').update(h264Source).digest('hex') !== H264_SHA256
  ) {
    throw new Error('Review the pinned mediasoup ORTC/H264 bundle contract before updating');
  }
}

/**
 * Keep the pinned Device graph free of unused public exports without replacing
 * any implementation or changing another module's resolution.
 * @returns {import('vite').Plugin}
 */
export function mediasoupRuntime() {
  const base = new URL('../node_modules/mediasoup-client/', import.meta.url);
  const remoteSdp = realpathSync(fileURLToPath(new URL('lib/handlers/sdp/RemoteSdp.js', base)));
  const version = checkedVersion(
    /** @type {unknown} */ (JSON.parse(readFileSync(new URL('package.json', base), 'utf8'))),
    readFileSync(remoteSdp),
  );
  const ortc = realpathSync(fileURLToPath(new URL('lib/ortc.js', base)));
  const dependencyRequire = createRequire(ortc);
  const h264 = realpathSync(dependencyRequire.resolve('h264-profile-level-id'));
  checkedH264Import(
    /** @type {unknown} */ (
      JSON.parse(
        readFileSync(dependencyRequire.resolve('h264-profile-level-id/package.json'), 'utf8'),
      )
    ),
    readFileSync(ortc),
    readFileSync(h264),
  );
  return {
    name: 'simplestchat-mediasoup-runtime',
    enforce: 'pre',
    resolveId(source, importer) {
      if (source === '../../' && importer === remoteSdp) return VERSION_MODULE;
      return source === 'h264-profile-level-id' && importer === ortc ? H264_MODULE : null;
    },
    load(id) {
      if (id === VERSION_MODULE) return `export const version = ${JSON.stringify(version)};`;
      return id === H264_MODULE
        ? `export { isSameProfile, generateProfileLevelIdStringForAnswer } from ${JSON.stringify(h264)};`
        : null;
    },
  };
}
