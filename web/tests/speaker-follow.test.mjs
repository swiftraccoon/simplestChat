import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

async function fixture() {
  let now = 0;
  let nextId = 0;
  const timers = new Map();
  const changes = [];
  const storage = new Map();
  const policy = await loadTypeScript('src/receive-policy.ts', {
    globals: {
      performance: { now: () => now },
      localStorage: {
        getItem: (key) => storage.get(key) ?? null,
        setItem: (key, value) => storage.set(key, value),
      },
      setTimeout(callback, delay) {
        const id = ++nextId;
        timers.set(id, { callback, at: now + delay });
        return id;
      },
      clearTimeout: (id) => timers.delete(id),
    },
  });
  const focus = new policy.SpeakerFollow((id) => changes.push([now, id]));
  return {
    focus,
    changes,
    timers,
    policy,
    storage,
    advance(ms) {
      const until = now + ms;
      while (timers.size) {
        const [id, timer] = [...timers].sort((left, right) => left[1].at - right[1].at)[0];
        if (timer.at > until) break;
        now = timer.at;
        timers.delete(id);
        timer.callback();
      }
      now = until;
    },
  };
}

test('speaker focus settles for 1.5 seconds and holds for at least five seconds', async () => {
  const { focus, changes, advance, timers } = await fixture();
  focus.note('first');
  advance(1_499);
  assert.equal(focus.participantId, null);
  focus.note('first');
  advance(1);
  assert.equal(focus.participantId, 'first', 'repeat observations do not restart stability');
  focus.note('second');
  advance(4_999);
  assert.equal(focus.participantId, 'first');
  advance(1);
  assert.deepEqual(changes, [
    [1_500, 'first'],
    [6_500, 'second'],
  ]);
  assert.equal(timers.size, 0);
});

test('brief interruptions and rapid competing speakers preserve focus with one pending timer', async () => {
  const { focus, changes, advance, timers } = await fixture();
  focus.note('first');
  advance(1_500);
  for (let index = 0; index < 100; index++) {
    focus.note(index % 2 ? 'second' : 'third');
    advance(100);
    assert.equal(timers.size, 1);
  }
  assert.deepEqual(changes, [[1_500, 'first']]);
  focus.note('first');
  advance(10_000);
  assert.equal(timers.size, 0);
  assert.deepEqual(changes, [[1_500, 'first']]);
});

test('silence cancels a candidate but retains an established camera', async () => {
  const { focus, changes, advance, timers } = await fixture();
  focus.note('first');
  advance(1_500);
  focus.note('second');
  focus.note(null);
  advance(10_000);
  assert.equal(focus.participantId, 'first');
  assert.equal(timers.size, 0);
  assert.deepEqual(changes, [[1_500, 'first']]);
});

test('reset and departure cancel stale timers and clear focused identity once', async () => {
  const { focus, changes, advance, timers } = await fixture();
  focus.note('first');
  focus.forget('first');
  advance(5_000);
  assert.equal(focus.participantId, null);
  focus.note('second');
  advance(1_500);
  focus.note('third');
  focus.forget('second');
  focus.reset();
  advance(10_000);
  assert.deepEqual(changes, [
    [6_500, 'second'],
    [6_500, null],
  ]);
  assert.equal(timers.size, 0);
});

test('the opt-in preference defaults off and tolerates blocked browser storage', async () => {
  const { policy, storage } = await fixture();
  assert.equal(policy.loadFollowSpeaker(), false);
  policy.saveFollowSpeaker(true);
  assert.equal(policy.loadFollowSpeaker(), true);
  policy.saveFollowSpeaker(false);
  assert.equal(policy.loadFollowSpeaker(), false);
  storage.set('simplestchat.followSpeaker', 'yes');
  assert.equal(policy.loadFollowSpeaker(), false);
  const blocked = await loadTypeScript('src/receive-policy.ts', {
    globals: {
      localStorage: {
        getItem() {
          throw new Error('blocked');
        },
        setItem() {
          throw new Error('blocked');
        },
      },
    },
  });
  assert.equal(blocked.loadFollowSpeaker(), false);
  assert.doesNotThrow(() => blocked.saveFollowSpeaker(true));
});

test('followed cameras yield to pins, picture-in-picture and visible shares', async () => {
  const { policy } = await fixture();
  const selected = policy.selectIncomingVideos(
    [
      { id: 'camera', source: 'camera', visible: true, pinned: false, hidden: false },
      {
        id: 'speaker',
        source: 'camera',
        visible: false,
        followedSpeaker: true,
        pinned: false,
        hidden: false,
      },
      { id: 'share', source: 'screen', visible: true, pinned: false, hidden: false },
      { id: 'pin', source: 'camera', visible: false, pinned: true, hidden: false },
      {
        id: 'pip',
        source: 'camera',
        visible: false,
        pictureInPicture: true,
        pinned: false,
        hidden: false,
      },
    ],
    'data-saver',
    true,
    new Set(['camera']),
  );
  assert.deepEqual([...selected], ['pip', 'pin', 'share', 'speaker']);
  assert.deepEqual(
    [
      ...policy.selectIncomingVideos(
        [{ id: 'speaker', visible: false, followedSpeaker: true, pinned: false, hidden: true }],
        'balanced',
        true,
        new Set(),
      ),
    ],
    [],
  );
  assert.deepEqual(
    [
      ...policy.selectIncomingVideos(
        [{ id: 'speaker', visible: false, followedSpeaker: true, pinned: false, hidden: false }],
        'balanced',
        false,
        new Set(),
      ),
    ],
    [],
  );
});
