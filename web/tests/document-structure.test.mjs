import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const html = await readFile(new URL('../index.html', import.meta.url), 'utf8');

test('the document exposes landmarks, a polite live region for toasts and page metadata', () => {
  assert.match(html, /<main\b/, 'screens live inside a main landmark');
  assert.match(html, /<nav[^>]*aria-label="Call controls"/);
  assert.match(html, /<aside[^>]*id="sidebar"/);
  assert.match(
    html,
    /<div id="toast-container" role="status" aria-live="polite"/,
    'toasts are announced without stealing focus',
  );
  assert.match(html, /<meta\s+name="description"/);
  assert.match(html, /<meta\s+name="theme-color"/);
  assert.match(html, /<link\s+rel="icon"/);
  assert.match(html, /id="room-screen"[^>]*tabindex="-1"/, 'the room can receive focus on entry');
});

test('room chrome has homes for room actions, diagnostics and the phone panel toggle', () => {
  assert.match(html, /id="room-actions"/);
  assert.match(html, /id="home-tools"/);
  assert.match(html, /id="room-tools-right"/);
  assert.match(html, /id="sidebar-collapse"/);
  assert.match(html, /data-tab="users"[^>]*>\s*People\s*</, 'one word for the roster everywhere');
});

test('dialogs share one close affordance and one label style', () => {
  assert.doesNotMatch(html, /&times;/, 'close buttons are labelled, not a multiplication sign');
  assert.match(html, /id="login-close"[^>]*aria-label="Close sign in"/);
  assert.match(html, /id="rs-password-remove"/);
  assert.doesNotMatch(
    html,
    />\s*Done\s*<\/button>/,
    'apply-on-change dialogs do not pretend to commit',
  );
});

test('creating a room asks for the name first and uses the same vocabulary as room settings', () => {
  const name = html.indexOf('id="cr-name"');
  const id = html.indexOf('id="cr-id"');
  assert.ok(name > 0 && id > name, 'display name precedes the room ID');
  for (const label of ['Lobby Enabled', 'Guests Allowed', 'Unlisted (hidden from room browser)'])
    assert.equal(html.includes(label), false, `${label} is renamed`);
  for (const label of ['Waiting room', 'Allow guests', 'Unlisted room', 'Moderated participation'])
    assert.ok(html.includes(label), `${label} appears in create room`);
});
