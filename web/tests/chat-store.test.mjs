import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const { ChatStore, ConversationInputs } = await loadTypeScript('src/chat-store.ts');
const entry = (id, extra = {}) => ({
  messageId: `server-${id}`,
  clientMessageId: `client-${id}`,
  participantId: 'alice',
  participantName: 'Alice',
  content: `Message ${id}`,
  sentAt: '2026-01-01T00:00:00.000Z',
  ...extra,
});
function store(...limits) {
  const value = new ChatStore(...limits);
  value.localId = 'local';
  return value;
}

test('public and private optimistic messages deduplicate acknowledgments, events, and replay', () => {
  const value = store();
  for (const recipientId of [undefined, 'alice']) {
    const message = entry(recipientId ?? 'public', {
      participantId: 'local',
      recipientId,
      recipientName: 'Alice',
    });
    assert.equal(
      value.pending({ ...message, messageId: `pending:${message.clientMessageId}` }),
      true,
    );
    value.fail(message.clientMessageId, 'Unconfirmed');
    assert.equal(value.receive(message), false);
    assert.equal(value.receive(message, true), false);
  }
  assert.equal(value.messages.length, 2);
  assert.ok(
    value.messages.every((message) => message.status === 'sent' && message.error === undefined),
  );
  assert.equal(value.names.get('alice'), 'Alice');
  assert.equal(value.unread.size, 0);
});

test('PM privacy and conversation identity are enforced by the session store', () => {
  const value = store();
  assert.equal(value.receive(entry('secret', { recipientId: 'bob' })), false);
  assert.equal(value.receive(entry('valid', { recipientId: 'local' })), true);
  assert.equal(
    value.receive(entry('valid', { recipientId: undefined })),
    false,
    'same server ID cannot change audience',
  );
  assert.equal(value.messages.length, 1);
  value.reset();
  assert.equal(value.receive(entry('no-membership')), false);
});

test('replay merges chronologically and unseen entries, not duplicates, contribute unread', () => {
  const value = store();
  const newer = entry('newer', { recipientId: 'local', sentAt: '2026-01-02T00:00:00Z' });
  const older = entry('older', { recipientId: 'local' });
  value.receive(newer);
  value.receive(older, true);
  value.receive(newer, true);
  assert.deepEqual(
    value.messages.map((message) => message.messageId),
    ['server-older', 'server-newer'],
  );
  assert.equal(value.unread.get('alice'), 2);
  value.open('alice');
  assert.equal(value.unread.size, 0);
  value.receive(older, true);
  assert.equal(value.unread.size, 0);
  value.markUnread(newer);
  value.markUnread(newer);
  assert.equal(value.unread.get('alice'), 1);
  value.markRead('alice');
  assert.equal(value.unread.size, 0);
});

test('message count and character bounds also apply to ACK replacement and failures', () => {
  const value = store(2, 500);
  for (let id = 0; id < 3; id++) value.receive(entry(id, { recipientId: 'local' }));
  assert.equal(value.messages.length, 2);
  assert.equal(value.unread.get('alice'), 2);
  value.receive(entry(2, { recipientId: 'local', content: 'x'.repeat(600) }));
  assert.equal(value.messages.length, 0);
  assert.equal(value.unread.size, 0);
  value.pending(entry('pending', { participantId: 'local' }));
  value.fail('client-pending', 'x'.repeat(2000));
  assert.equal(value.messages.length, 0);
});

test('closing a PM suppresses its late ACK and replay without hiding new messages or trusting clocks', () => {
  const value = store();
  const pending = entry('pending', {
    participantId: 'local',
    recipientId: 'alice',
    sentAt: '2099-01-01T00:00:00Z',
  });
  value.pending(pending);
  value.open('alice', 'Alice');
  value.close('alice');
  assert.equal(value.receive({ ...pending, sentAt: '2026-01-01T00:00:00Z' }), false);
  assert.equal(value.names.size, 0);
  assert.equal(
    value.receive(entry('new', { recipientId: 'local', sentAt: '2000-01-01T00:00:00Z' })),
    true,
  );
  assert.equal(value.names.get('alice'), 'Alice');
  assert.equal(value.unread.get('alice'), 1);
  assert.equal(value.active, 'public');
});

test('conversation metadata is bounded even for empty opened and closed conversations', () => {
  const value = store();
  for (let id = 0; id < 150; id++) value.open(`guest-${id}`, `Guest ${id}`);
  assert.equal(value.names.size, 100);
  assert.equal(value.names.has('guest-149'), true);
  assert.equal(value.active, 'guest-149');
  value.receive(entry('offline', { recipientId: 'local' }));
  assert.equal(
    value.names.get('alice'),
    'Alice',
    'no presence event erases the offline conversation',
  );
  value.reset('other-account');
  assert.equal(value.names.size, 0);
  assert.equal(value.messages.length, 0);
  assert.equal(value.unread.size, 0);
});

test('drafts and sent-input recall never cross public/private conversation boundaries', () => {
  const inputs = new ConversationInputs();
  inputs.save('public', 'public draft');
  inputs.save('alice', 'private draft');
  inputs.sent('alice', 'private sent message');
  assert.equal(inputs.draft('public'), 'public draft');
  assert.equal(inputs.recall('public', 'up', ''), '');
  assert.equal(inputs.recall('alice', 'up', 'private unfinished'), 'private sent message');
  assert.equal(inputs.recall('alice', 'down', ''), 'private unfinished');
  inputs.save('alice', 'generated @mention 😀');
  assert.equal(inputs.isRecalling('alice'), false);
  assert.equal(inputs.draft('alice'), 'generated @mention 😀');
  inputs.close('alice');
  assert.equal(inputs.recall('alice', 'up', ''), '');
  inputs.reset();
  assert.equal(inputs.draft('public'), '');
});

test('input history, draft length, conversation metadata, and total composition memory are bounded', () => {
  const inputs = new ConversationInputs();
  for (let id = 0; id < 80; id++) inputs.sent('public', `sent-${id}`);
  for (let id = 0; id < 80; id++) inputs.recall('public', 'up', '');
  assert.equal(inputs.recall('public', 'up', ''), 'sent-30');
  inputs.save('public', 'x'.repeat(3000));
  assert.equal(inputs.draft('public').length, 2000);
  for (let id = 0; id < 150; id++) inputs.save(`guest-${id}`, 'private');
  assert.ok(inputs.conversations.size <= 100);
  for (let id = 0; id < 100; id++)
    for (let message = 0; message < 5; message++) inputs.sent(`guest-${id}`, 'x'.repeat(2000));
  const characters = [...inputs.conversations.values()].reduce(
    (total, value) =>
      total + value.draft.length + value.beforeRecall.length + value.history.join('').length,
    0,
  );
  assert.ok(characters <= 256 * 1024);
});
