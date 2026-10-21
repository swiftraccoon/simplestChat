import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';

const shared = JSON.parse(
  await readFile(new URL('./browser-receive-policy-cases.json', import.meta.url), 'utf8'),
);
const policy = await loadTypeScript('src/receive-policy.ts');

test('the load generator receive limits match the shipped video policy', () => {
  assert.deepEqual(shared.videoLimits, policy.VIDEO_RECEIVE_LIMITS);
  for (const mode of Object.keys(shared.videoLimits))
    assert.equal(policy.normalizeReceiveMode(mode), mode);
});

test('shared room budgets count selected videos and every published microphone', () => {
  for (const row of shared.roomCases) {
    const candidates = row.videoPublished
      ? Array.from({ length: row.remotePublishers }, (_, index) => ({
          id: `camera-${index}`,
          source: 'camera',
          visible: true,
          pinned: false,
          hidden: false,
        }))
      : [];
    const videos = policy.selectIncomingVideos(candidates, row.mode, true, new Set()).size;
    const audio = row.audioPublished ? row.remotePublishers : 0;
    assert.equal(audio + videos, row.consumers, row.name);
    assert.equal(audio + videos <= shared.consumerLimit, row.valid, row.name);
  }
});
