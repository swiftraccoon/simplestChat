import assert from 'node:assert/strict';
import test from 'node:test';
import impairment from '../e2e/presentation-impairment.cjs';

const clean = () => [{ kind: 'noqueue', handle: '0:', root: true, refcnt: 2, options: {} }];
const root = () => ({
  kind: 'prio',
  handle: '1:',
  root: true,
  refcnt: 2,
  options: { bands: 3, priomap: Array(16).fill(0), multiqueue: false },
});
const netem = () => ({
  kind: 'netem',
  handle: '30:',
  parent: '1:3',
  options: {
    limit: 200000,
    delay: { delay: 0.03, jitter: 0, correlation: 0 },
    rate: { rate: 100000, packetoverhead: 0, cellsize: 0, celloverhead: 0 },
    ecn: false,
    gap: 0,
  },
});
// Native iproute2 6.1 output repeats match keys; its JSON hex is unpadded.
const filters = `[
  {"protocol":"ip","pref":1,"kind":"u32","chain":0},
  {"protocol":"ip","pref":1,"kind":"u32","chain":0,"options":{"fh":"800:","ht_divisor":1}},
  {"protocol":"ip","pref":1,"kind":"u32","chain":0,"options":{
    "fh":"800::800","order":2048,"key_ht":"800","bkt":"0","flowid":"1:3","not_in_hw":true,
    "match":{"value":"110000","mask":"ff0000","offmask":"","off":8},
    "match":{"value":"a0320000","mask":"ffff0000","offmask":"","off":20}
  }}
]`;

function fixture(options = {}) {
  const state = { qdiscs: clean(), filters: '[]' };
  const calls = [],
    records = [];
  let reads = 0,
    additions = 0;
  const operate = impairment.createPresentationImpairment({
    port: '41010',
    artifacts: '/unused-mocked-artifacts',
    execute(command, args, limits) {
      calls.push({ command, args: [...args], limits });
      if (command === 'tc') {
        reads++;
        if (reads === options.failReadAt) throw new Error('private network diagnostic');
        if (options.readOverride) return options.readOverride(args[1], state);
        return args[1] === 'qdisc' ? JSON.stringify(state.qdiscs) : state.filters;
      }
      assert.equal(command, 'sudo');
      assert.equal(args[0], 'tc');
      if (args.includes('add')) {
        additions++;
        if (additions === options.failAddAt) throw new Error('private install diagnostic');
        if ((additions - 1) % 3 === 0) state.qdiscs = [root()];
        else if ((additions - 1) % 3 === 1) state.qdiscs.push(netem());
        else {
          state.filters = filters;
          options.afterInstall?.(state);
        }
      } else {
        assert.deepEqual(args, ['tc', 'qdisc', 'del', 'dev', 'lo', 'root', 'handle', '1:']);
        if (!options.keepAfterDelete) {
          state.qdiscs = clean();
          state.filters = '[]';
        }
        if (options.failDelete) throw new Error('private cleanup diagnostic');
      }
      return '';
    },
    persist(value) {
      records.splice(0, records.length, ...structuredClone(value));
      if (options.failPersist) throw new Error('private evidence path');
    },
  });
  return {
    state,
    calls,
    records,
    operate,
    mutations: () => calls.filter((call) => call.command === 'sudo'),
  };
}

test('verified native-shaped profile uses unchanged bounded additive commands and proven idempotent clear', () => {
  const f = fixture();
  f.operate('apply');
  assert.deepEqual(f.records[0].before, { state: 'clean', qdiscs: 1, filters: 0, netem: false });
  assert.deepEqual(f.records[0].after, {
    state: 'fixture-shaped',
    qdiscs: 2,
    filters: 3,
    netem: true,
  });
  assert.equal(f.records[0].passed, true);
  const install = f.mutations();
  assert.equal(install.length, 3);
  assert.deepEqual(install[0].args, [
    'tc',
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
  ]);
  assert.deepEqual(install[1].args, [
    'tc',
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
  ]);
  assert.deepEqual(install[2].args, [
    'tc',
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
    '41010',
    '0xffff',
    'flowid',
    '1:3',
  ]);
  for (const call of f.calls) {
    assert.equal(call.limits.timeout, 5000);
    assert.equal(call.limits.maxBuffer, 65536);
    assert.deepEqual(call.limits.stdio, ['ignore', 'pipe', 'pipe']);
  }
  f.operate('clear');
  assert.equal(f.records[1].after.state, 'clean');
  assert.equal(f.records[1].passed, true);
  f.operate('clear');
  assert.equal(f.records[2].command, 'not-run');
  assert.equal(f.mutations().length, 4);
  f.operate('apply');
  f.operate('clear');
  assert.equal(f.records[4].passed, true, 'a cleared instance can install its next owned phase');
});

test('pre-existing identical or foreign state is never claimed or deleted by a fresh instance', () => {
  for (const state of [
    { qdiscs: [root(), netem()], filters },
    { qdiscs: [{ kind: 'fq', handle: '7:', root: true }], filters: '[]' },
    { qdiscs: clean(), filters: '[{"kind":"bpf"}]' },
  ]) {
    const f = fixture();
    Object.assign(f.state, state);
    assert.throws(() => f.operate('apply'), /pre-existing-network-state/);
    assert.throws(() => f.operate('clear'), /unowned-or-changed-network-state/);
    assert.equal(f.mutations().length, 0);
  }
});

test('failed partial own installs retain cleanup authority from the verified clean baseline', () => {
  for (const failAddAt of [2, 3]) {
    const f = fixture({ failAddAt });
    assert.throws(() => f.operate('apply'), /apply-command-failed/);
    assert.equal(f.records[0].command, 'failed');
    f.operate('clear');
    assert.equal(f.records[1].passed, true);
    assert.equal(f.records[1].after.state, 'clean');
  }
});

test('transient post-install inspection failure still permits exact owned-profile cleanup', () => {
  const f = fixture({ failReadAt: 3 });
  assert.throws(() => f.operate('apply'), /state-inspection-failed/);
  assert.equal(f.records[0].verification, 'failed');
  f.operate('clear');
  assert.equal(f.records[1].after.state, 'clean');
  assert.equal(f.records[1].passed, true);
});

test('post-install inspection uncertainty cannot claim a different or concurrently changed profile', () => {
  for (const change of [
    (state) => {
      state.qdiscs.pop();
      state.filters = '[]';
    },
    (state) => {
      state.qdiscs[1].options.rate.rate = 200000;
    },
    (state) => {
      state.filters = state.filters.replace('110000', '60000');
    },
    (state) => {
      state.filters = state.filters.replace('a0320000', 'a0330000');
    },
  ]) {
    const f = fixture({ failReadAt: 3 });
    assert.throws(() => f.operate('apply'), /state-inspection-failed/);
    change(f.state);
    assert.throws(() => f.operate('clear'), /unowned-or-changed-network-state/);
    assert.equal(f.mutations().length, 3);
  }
});

test('applied profile verification checks exact netem parameters and every native repeated u32 match', () => {
  for (const change of [
    (state) => {
      state.qdiscs[1].options.rate.rate = 800000;
    },
    (state) => {
      state.qdiscs[1].options.delay.delay = 30;
    },
    (state) => {
      state.qdiscs[1].options.delay.jitter = 0.001;
    },
    (state) => {
      state.qdiscs[1].options.limit = 1000;
    },
    (state) => {
      state.qdiscs[1].options['loss-random'] = { loss: 0.01 };
    },
    (state) => {
      state.filters = state.filters.replace('110000', '60000');
    },
    (state) => {
      state.filters = state.filters.replace('a0320000', 'a0330000');
    },
    (state) => {
      state.filters = state.filters.replace('ffff0000', 'fff00000');
    },
    (state) => {
      state.filters = state.filters.replace('"off":20', '"off":22');
    },
    (state) => {
      state.filters = state.filters.replace('"flowid":"1:3"', '"flowid":"1:2"');
    },
    (state) => {
      state.filters = state.filters.replace('"order":2048', '"action":"foreign","order":2048');
    },
  ]) {
    const f = fixture({ afterInstall: change });
    assert.throws(() => f.operate('apply'), /apply-state-unverified/);
    assert.throws(() => f.operate('clear'), /unowned-or-changed-network-state/);
    assert.equal(f.mutations().length, 3, 'reject uncertain foreign state without deletion');
  }
});

test('a changed filter or qdisc identity after successful install blocks cleanup without deleting foreign state', () => {
  const f = fixture();
  f.operate('apply');
  f.state.filters = f.state.filters.replace('800::800', '800::801');
  assert.throws(() => f.operate('clear'), /unowned-or-changed-network-state/);
  assert.equal(f.mutations().length, 3);
  assert.equal(f.records[1].passed, false);
});

test('changing dynamic root counters does not invalidate ownership', () => {
  const f = fixture();
  f.operate('apply');
  f.state.qdiscs[0].refcnt = 7;
  f.state.qdiscs[0].bytes = 123456;
  f.operate('clear');
  assert.equal(f.records[1].passed, true);
});

test('zero-exit cleanup that leaves shaping installed fails its mandatory post-clear inspection', () => {
  const f = fixture({ keepAfterDelete: true });
  f.operate('apply');
  assert.throws(() => f.operate('clear'), /clear-state-unverified/);
  assert.equal(f.records[1].command, 'succeeded');
  assert.equal(f.records[1].after.state, 'fixture-shaped');
  assert.equal(f.records[1].passed, false);
});

test('nonzero cleanup remains a failure even if kernel state became clean, and evidence is sanitized', () => {
  const f = fixture({ failDelete: true });
  f.operate('apply');
  assert.throws(() => f.operate('clear'), /clear-command-failed/);
  assert.equal(f.records[1].after.state, 'clean');
  assert.equal(f.records[1].passed, false);
  assert.ok(!JSON.stringify(f.records).includes('private'));
  f.operate('clear');
  assert.equal(f.records[2].command, 'not-run');
});

test('missing, malformed, excessive, or oversized state never reaches a mutating command', () => {
  for (const output of [
    'not-json',
    'null',
    JSON.stringify(Array(17).fill({})),
    ' '.repeat(65537),
  ]) {
    const f = fixture({ readOverride: () => output });
    assert.throws(() => f.operate('apply'), /state-inspection-failed/);
    assert.equal(f.mutations().length, 0);
    assert.equal(f.records[0].error, 'state-inspection-failed');
  }
});

test('artifact failures do not replace an original operation error or publish raw command details', () => {
  const f = fixture({ failAddAt: 2, failPersist: true });
  assert.throws(() => f.operate('apply'), /apply-command-failed/);
  assert.equal(f.records[0].error, 'apply-command-failed');
  assert.throws(() => f.operate('clear'), /evidence-write-failed/);
  assert.equal(f.records[1].after.state, 'clean');
  assert.ok(!JSON.stringify(f.records).includes('private'));
});

test('failed install command survives a simultaneous verification and evidence-write failure', () => {
  const f = fixture({ failAddAt: 2, failReadAt: 3, failPersist: true });
  assert.throws(() => f.operate('apply'), /apply-command-failed/);
  assert.equal(f.records[0].verification, 'failed');
  assert.equal(f.records[0].error, 'apply-command-failed');
  assert.throws(() => f.operate('clear'), /evidence-write-failed/);
  assert.equal(f.records[1].after.state, 'clean');
});

test('invalid ports and repeated operations remain bounded and fail before command execution', () => {
  for (const port of ['0', '65536', '41010;id', '', '000001'])
    assert.throws(() => impairment.createPresentationImpairment({ port }), /invalid-port/);
  const f = fixture();
  assert.throws(() => f.operate('replace'), /invalid-action/);
  for (let i = 0; i < 16; i++) f.operate('clear');
  const count = f.calls.length;
  assert.throws(() => f.operate('clear'), /invalid-action/);
  assert.equal(f.calls.length, count);
});
