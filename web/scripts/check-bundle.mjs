import { lstat, readFile, readdir, realpath } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

/** @typedef {'javascript' | 'styles' | 'html'} AssetGroup */
/** @typedef {{ bytes: number, gzipBytes: number }} Size */
/** @typedef {Record<AssetGroup, Size>} Sizes */

const GROUPS = /** @type {const} */ (['javascript', 'styles', 'html']);
const MAX_FILE_BYTES = 16 * 1024 * 1024;

/** @param {unknown} value @returns {value is Record<string, unknown>} */
function isRecord(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** @returns {Sizes} */
function emptySizes() {
  return {
    javascript: { bytes: 0, gzipBytes: 0 },
    styles: { bytes: 0, gzipBytes: 0 },
    html: { bytes: 0, gzipBytes: 0 },
  };
}

/**
 * Reject missing, misspelled or nonpositive limits so configuration mistakes
 * cannot silently disable a gate. Limits are bytes, not decimal kilobytes.
 * @param {unknown} value
 * @returns {Sizes}
 */
export function parseBudgets(value) {
  if (!isRecord(value) || Object.keys(value).length !== GROUPS.length) {
    throw new Error('Bundle budgets must contain exactly javascript, styles and html');
  }
  const result = emptySizes();
  for (const group of GROUPS) {
    const limits = value[group];
    if (!isRecord(limits) || Object.keys(limits).length !== 2) {
      throw new Error(`Invalid ${group} budget: expected bytes and gzipBytes`);
    }
    for (const key of /** @type {const} */ (['bytes', 'gzipBytes'])) {
      const limit = limits[key];
      if (typeof limit !== 'number' || !Number.isSafeInteger(limit) || limit <= 0) {
        throw new Error(`Invalid ${group}.${key} budget: expected a positive safe integer`);
      }
      result[group][key] = limit;
    }
    if (result[group].gzipBytes > result[group].bytes) {
      throw new Error(`Invalid ${group} budget: gzipBytes must not exceed bytes`);
    }
  }
  return result;
}

/** @param {string} filename @returns {AssetGroup | undefined} */
function assetGroup(filename) {
  switch (path.extname(filename).toLowerCase()) {
    case '.js':
    case '.mjs':
    case '.cjs':
      return 'javascript';
    case '.css':
      return 'styles';
    case '.html':
      return 'html';
    default:
      return undefined;
  }
}

/**
 * Count every emitted JS, CSS and HTML file, including lazy chunks and help.
 * Gzip level 6 is measured separately per file, then summed by asset group.
 * This is a deterministic size gate, not a network or runtime benchmark.
 * Reject symlinks/special files instead of traversing outside the build output.
 * @param {string} directory
 * @returns {Promise<Sizes>}
 */
export async function measureBundle(directory) {
  const root = await lstat(directory);
  if (!root.isDirectory() || root.isSymbolicLink()) {
    throw new Error('Bundle output must be a regular directory');
  }
  const sizes = emptySizes();
  const required = new Set(['index.html', 'help.html']);

  /** @param {string} relative @returns {Promise<void>} */
  async function visit(relative) {
    const entries = await readdir(path.join(directory, relative), { withFileTypes: true });
    // Stable traversal makes diagnostics reproducible across filesystems.
    entries.sort((left, right) => left.name.localeCompare(right.name, 'en'));
    for (const entry of entries) {
      const filename = path.join(relative, entry.name);
      if (entry.isDirectory()) {
        await visit(filename);
        continue;
      }
      if (!entry.isFile()) {
        throw new Error(`Bundle contains a symlink or special file: ${filename}`);
      }
      const group = assetGroup(filename);
      if (!group) continue;
      const absolute = path.join(directory, filename);
      const metadata = await lstat(absolute);
      if (!metadata.isFile() || metadata.size > MAX_FILE_BYTES) {
        throw new Error(`Bundle asset must be a regular file of at most 16 MiB: ${filename}`);
      }
      const contents = await readFile(absolute);
      if (required.has(filename) && contents.byteLength === 0) {
        throw new Error(`Bundle required page is empty: ${filename}`);
      }
      sizes[group].bytes += contents.byteLength;
      sizes[group].gzipBytes += gzipSync(contents, { level: 6 }).byteLength;
      required.delete(filename);
    }
  }

  await visit('');
  if (required.size > 0) {
    throw new Error(`Bundle is missing required pages: ${[...required].join(', ')}`);
  }
  if (sizes.javascript.bytes === 0 || sizes.styles.bytes === 0) {
    throw new Error('Bundle must contain nonempty JavaScript and CSS assets');
  }
  return sizes;
}

/** @param {Sizes} sizes @param {Sizes} budgets @returns {string[]} */
export function budgetFailures(sizes, budgets) {
  const failures = [];
  for (const group of GROUPS) {
    for (const key of /** @type {const} */ (['bytes', 'gzipBytes'])) {
      if (sizes[group][key] > budgets[group][key]) {
        failures.push(`${group}.${key}: ${sizes[group][key]} exceeds ${budgets[group][key]} bytes`);
      }
    }
  }
  return failures;
}

/** @returns {Promise<void>} */
async function main() {
  /** @type {unknown} */
  const config = JSON.parse(
    await readFile(new URL('../bundle-budget.json', import.meta.url), 'utf8'),
  );
  const budgets = parseBudgets(config);
  const sizes = await measureBundle(fileURLToPath(new URL('../dist', import.meta.url)));
  for (const group of GROUPS) {
    console.log(
      `${group}: ${sizes[group].bytes}/${budgets[group].bytes} bytes; ` +
        `gzip ${sizes[group].gzipBytes}/${budgets[group].gzipBytes} bytes`,
    );
  }
  const failures = budgetFailures(sizes, budgets);
  if (failures.length > 0) throw new Error(`Bundle budget exceeded:\n${failures.join('\n')}`);
}

// Node resolves module symlinks (including macOS /var -> /private/var); argv
// retains the caller's spelling. Compare real paths without running on import.
if (
  process.argv[1] &&
  (await realpath(process.argv[1]).catch(() => undefined)) === fileURLToPath(import.meta.url)
) {
  main().catch(
    /** @param {unknown} error */ (error) => {
      console.error(error instanceof Error ? error.message : 'Bundle size check failed');
      process.exitCode = 1;
    },
  );
}
