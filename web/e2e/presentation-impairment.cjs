/** Observe and retire only the loopback profile installed by this fixture instance. */
const { execFileSync } = require('node:child_process');
const { createHash } = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');

const commandOptions = {
  encoding: 'utf8',
  stdio: ['ignore', 'pipe', 'pipe'],
  timeout: 5000,
  maxBuffer: 65536,
};
const failure = (code) => new Error(`Presentation impairment: ${code}`);

function canonical(value, depth = 0) {
  if (depth > 12) throw failure('invalid-network-state');
  if (Array.isArray(value)) return value.map((item) => canonical(item, depth + 1));
  if (value && typeof value === 'object')
    return Object.fromEntries(
      Object.keys(value)
        .sort()
        .map((key) => [key, canonical(value[key], depth + 1)]),
    );
  return value;
}

function filterJson(output) {
  // iproute2 6.1 prints one repeated "match" object per u32 selector.
  // Preserve every selector instead of JSON.parse silently keeping the last.
  if (/"fixtureMatch[0-9]+"\s*:/.test(output)) throw failure('invalid-network-state');
  let index = 0;
  return JSON.parse(output.replace(/"match"\s*:/g, () => `"fixtureMatch${index++}":`));
}

const only = (value, keys) =>
  value && typeof value === 'object' && Object.keys(value).every((key) => keys.includes(key));

function networkState(qdiscs, filters, port) {
  if (
    !Array.isArray(qdiscs) ||
    !Array.isArray(filters) ||
    qdiscs.length > 16 ||
    filters.length > 16 ||
    [...qdiscs, ...filters].some((entry) => !entry || typeof entry !== 'object')
  )
    throw failure('invalid-network-state');
  const clean =
    filters.length === 0 &&
    (qdiscs.length === 0 ||
      (qdiscs.length === 1 &&
        qdiscs[0].root === true &&
        qdiscs[0].kind === 'noqueue' &&
        qdiscs[0].handle === '0:'));
  const roots = qdiscs.filter((entry) => entry.root === true);
  const root = roots[0];
  const netem = qdiscs.find(
    (entry) => entry.kind === 'netem' && entry.handle === '30:' && entry.parent === '1:3',
  );
  const netemOptions = netem?.options;
  // iproute2 6.1 q_netem prints seconds and json_print prints bytes/second.
  const validNetem =
    !netem ||
    (only(netemOptions, ['limit', 'delay', 'rate', 'ecn', 'gap']) &&
      netemOptions.limit === 200000 &&
      only(netemOptions.delay, ['delay', 'jitter', 'correlation']) &&
      Math.abs(netemOptions.delay.delay - 0.03) < 0.000001 &&
      netemOptions.delay.jitter === 0 &&
      netemOptions.delay.correlation === 0 &&
      only(netemOptions.rate, ['rate', 'packetoverhead', 'cellsize', 'celloverhead']) &&
      netemOptions.rate.rate === 100000 &&
      netemOptions.rate.packetoverhead === 0 &&
      netemOptions.rate.cellsize === 0 &&
      netemOptions.rate.celloverhead === 0 &&
      netemOptions.ecn === false &&
      netemOptions.gap === 0);
  const leaves = filters.filter((entry) => entry.options?.flowid !== undefined);
  const validFilterOptions = filters.every((entry) => {
    const options = entry.options;
    if (options === undefined || Object.keys(options).length === 0) return true;
    if (options.flowid === undefined)
      return only(options, ['fh', 'ht_divisor']) && options.ht_divisor === 1;
    return (
      Boolean(netem) &&
      Object.keys(options).every(
        (key) =>
          ['fh', 'order', 'key_ht', 'bkt', 'flowid', 'not_in_hw', 'in_hw'].includes(key) ||
          /^fixtureMatch[0-9]+$/.test(key),
      )
    );
  });
  const selectors = leaves.flatMap((entry) =>
    Object.entries(entry.options)
      .filter(([key]) => /^fixtureMatch[0-9]+$/.test(key))
      .map(([, value]) => value),
  );
  const match = (selector, value, mask, offset) =>
    only(selector, ['value', 'mask', 'offmask', 'off']) &&
    /^[0-9a-f]{1,8}$/.test(selector.value) &&
    /^[0-9a-f]{1,8}$/.test(selector.mask) &&
    Number.parseInt(selector.value, 16) === value &&
    Number.parseInt(selector.mask, 16) === mask &&
    selector.offmask === '' &&
    selector.off === offset;
  const validSelectors =
    leaves.length === 0 ||
    (leaves.length === 1 &&
      selectors.length === 2 &&
      selectors.some((entry) => match(entry, 0x00110000, 0x00ff0000, 8)) &&
      selectors.some((entry) => match(entry, Number(port) * 65536, 0xffff0000, 20)));
  const ownedShape =
    roots.length === 1 &&
    root.kind === 'prio' &&
    root.handle === '1:' &&
    root.options?.bands === 3 &&
    Array.isArray(root.options.priomap) &&
    root.options.priomap.length === 16 &&
    root.options.priomap.every((band) => band === 0) &&
    only(root.options, ['bands', 'priomap', 'multiqueue']) &&
    (root.options.multiqueue === undefined || root.options.multiqueue === false) &&
    validNetem &&
    validSelectors &&
    validFilterOptions &&
    filters.length <= 3 &&
    qdiscs.every((entry) => entry === root || entry === netem) &&
    filters.every(
      (entry) =>
        entry.kind === 'u32' &&
        entry.protocol === 'ip' &&
        entry.pref === 1 &&
        (entry.chain === undefined || entry.chain === 0) &&
        (entry.parent === undefined || entry.parent === '1:') &&
        (entry.options?.flowid === undefined || entry.options.flowid === '1:3'),
    );
  const pick = (entry, keys) =>
    Object.fromEntries(
      keys.filter((key) => entry[key] !== undefined).map((key) => [key, entry[key]]),
    );
  const identity = {
    qdiscs: qdiscs
      .map((entry) => canonical(pick(entry, ['kind', 'handle', 'root', 'parent', 'options'])))
      .map((entry) => JSON.stringify(entry))
      .sort(),
    filters: filters
      .map((entry) =>
        canonical(pick(entry, ['kind', 'parent', 'protocol', 'pref', 'chain', 'options'])),
      )
      .map((entry) => JSON.stringify(entry))
      .sort(),
  };
  return {
    clean,
    ownedShape,
    complete: ownedShape && Boolean(netem) && leaves.length === 1,
    signature: createHash('sha256').update(JSON.stringify(identity)).digest('hex'),
    evidence: {
      state: clean ? 'clean' : ownedShape ? 'fixture-shaped' : 'foreign',
      qdiscs: qdiscs.length,
      filters: filters.length,
      netem: Boolean(netem),
    },
  };
}

function createPresentationImpairment({
  port,
  artifacts,
  execute = execFileSync,
  persist = (records) => {
    fs.writeFileSync(
      path.join(artifacts, 'impairment-state.json'),
      `${JSON.stringify(records, null, 2)}\n`,
      {
        mode: 0o600,
      },
    );
  },
}) {
  if (!/^\d{1,5}$/.test(String(port)) || Number(port) < 1 || Number(port) > 65535)
    throw failure('invalid-port');
  let attempted = false;
  let installed = false;
  let signature = null;
  const records = [];
  const inspect = () => {
    try {
      const read = (kind) => {
        const output = execute('tc', ['-j', kind, 'show', 'dev', 'lo'], commandOptions);
        if (typeof output !== 'string' || Buffer.byteLength(output) > commandOptions.maxBuffer)
          throw failure('invalid-network-state');
        return kind === 'filter' ? filterJson(output) : JSON.parse(output);
      };
      return networkState(read('qdisc'), read('filter'), port);
    } catch {
      throw failure('state-inspection-failed');
    }
  };
  return (action) => {
    if (!['apply', 'clear'].includes(action) || records.length >= 16)
      throw failure('invalid-action');
    const record = { action, before: null, after: null, command: 'not-run', passed: false };
    let code = null;
    let operationError = null;
    try {
      const before = inspect();
      record.before = before.evidence;
      if (action === 'apply') {
        if (attempted || !before.clean) throw failure('pre-existing-network-state');
        // Retain ownership across an uncertain/partial install, starting only
        // from the independently observed clean interface.
        attempted = true;
        try {
          // Additive commands preserve the original profile without the
          // generic script's unconditional deletion of an existing root.
          for (const args of [
            [
              'qdisc',
              'add',
              'dev',
              'lo',
              'root',
              'handle',
              '1:',
              'prio',
              'bands',
              '3',
              'priomap',
              ...Array(16).fill('0'),
            ],
            [
              'qdisc',
              'add',
              'dev',
              'lo',
              'parent',
              '1:3',
              'handle',
              '30:',
              'netem',
              'limit',
              '200000',
              'delay',
              '30ms',
              'rate',
              '800kbit',
            ],
            [
              'filter',
              'add',
              'dev',
              'lo',
              'parent',
              '1:',
              'protocol',
              'ip',
              'prio',
              '1',
              'u32',
              'match',
              'ip',
              'protocol',
              '17',
              '0xff',
              'match',
              'ip',
              'sport',
              String(port),
              '0xffff',
              'flowid',
              '1:3',
            ],
          ])
            execute('sudo', ['tc', ...args], commandOptions);
          installed = true;
          record.command = 'succeeded';
        } catch {
          record.command = 'failed';
          code = 'apply-command-failed';
        }
        let after;
        try {
          after = inspect();
        } catch {
          record.verification = 'failed';
          throw failure(code || 'state-inspection-failed');
        }
        record.after = after.evidence;
        if (after.ownedShape) signature = after.signature;
        if (after.clean) attempted = false;
        if (code) throw failure(code);
        if (!after.complete) throw failure('apply-state-unverified');
      } else if (!before.clean) {
        if (
          !attempted ||
          !before.ownedShape ||
          (installed && !before.complete) ||
          (signature !== null && before.signature !== signature)
        )
          throw failure('unowned-or-changed-network-state');
        try {
          // Qualify the deletion with our handle; never reset an arbitrary root.
          execute(
            'sudo',
            ['tc', 'qdisc', 'del', 'dev', 'lo', 'root', 'handle', '1:'],
            commandOptions,
          );
          record.command = 'succeeded';
        } catch {
          record.command = 'failed';
          code = 'clear-command-failed';
        }
        let after;
        try {
          after = inspect();
        } catch {
          record.verification = 'failed';
          throw failure(code || 'state-inspection-failed');
        }
        record.after = after.evidence;
        if (after.clean) {
          attempted = false;
          installed = false;
          signature = null;
        }
        if (code) throw failure(code);
        if (!after.clean) throw failure('clear-state-unverified');
      } else {
        attempted = false;
        installed = false;
        signature = null;
        record.after = before.evidence;
      }
      record.passed = true;
    } catch (error) {
      record.error = /^Presentation impairment: [a-z-]+$/.test(error.message)
        ? error.message.slice('Presentation impairment: '.length)
        : 'operation-failed';
      operationError = failure(record.error);
    } finally {
      records.push(record);
      try {
        persist(records);
      } catch {
        // Artifact failure must never erase the failure that required cleanup.
        operationError ||= failure('evidence-write-failed');
      }
    }
    if (operationError) throw operationError;
  };
}

module.exports = { createPresentationImpairment };
