import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';

const { ChatStore, ConversationInputs } = await loadTypeScript('src/chat-store.ts');
const entry = (id, extra = {}) => ({
  messageId: `server-${id}`,
  clientMessageId: `client-${id}`,
  participantId: 'alice',
  participantName: 'Alice',
  revision: 0,
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
  assert.equal(
    value.unread.get('alice'),
    2,
    'opening a conversation does not prove its messages were visible',
  );
  value.markRead('alice');
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

test('removal scrubs quotes, ignores late reactions and cannot be undone by stale replay', () => {
  const value = store();
  const original = entry('original', {
    content: 'secret',
    reactions: [{ emoji: '👍', participantIds: ['local'] }],
  });
  const quote = entry('quote', {
    replyTo: {
      messageId: original.messageId,
      participantId: 'alice',
      participantName: 'Alice',
      excerpt: 'secret',
    },
  });
  value.receive(original);
  value.receive(quote);
  value.receive(entry('private', { recipientId: 'local', content: 'private' }));
  value.removeMessage(original.messageId, '2026-10-07T12:00:00Z');
  value.receive(original, true);
  value.receive(quote, true);
  assert.equal(value.messages[0].content, '');
  assert.equal(value.messages[0].removedAt, '2026-10-07T12:00:00Z');
  assert.deepEqual(value.messages[0].reactions, []);
  assert.equal(value.messages[1].replyTo.excerpt, 'Message removed');
  assert.equal(value.messages[2].content, 'private');
  assert.equal(
    value.setReactions(original.messageId, [{ emoji: '👍', participantIds: ['local'] }]),
    false,
  );
  value.reset('local');
  value.receive(original);
  assert.equal(value.messages[0].content, 'secret', 'removal IDs do not cross room identities');
});

test('removal arriving before its original still scrubs late content and quotes', () => {
  const value = store();
  value.removeMessage('server-original', '2026-10-07T12:00:00Z');
  value.receive(entry('original'));
  value.receive(
    entry('quote', {
      replyTo: {
        messageId: 'server-original',
        participantId: 'alice',
        participantName: 'Alice',
        excerpt: 'removed text',
      },
    }),
  );
  assert.equal(value.messages[0].content, '');
  assert.equal(value.messages[1].replyTo.excerpt, 'Message removed');
});

test('recovery prunes only captured confirmed public rows absent from authoritative replay', () => {
  const value = store();
  value.receive(entry('gone'));
  value.receive(entry('retained'));
  value.receive(entry('private', { recipientId: 'local' }));
  value.pending(entry('pending', { participantId: 'local' }));
  value.pending(entry('unknown', { participantId: 'local' }));
  value.fail('client-unknown', 'Unconfirmed', true);
  value.receive(entry('notice', { participantId: '', participantName: '' }));
  const captured = new Set(value.messages.map((message) => message.messageId));
  value.markUnread(value.messages[0]);
  value.receive(entry('new-live'));
  const discarded = value.prunePublicReplay(captured, new Set(['server-retained']));
  assert.deepEqual([...discarded], ['server-gone']);
  assert.deepEqual(
    value.messages.map((message) => message.messageId),
    [
      'server-retained',
      'server-private',
      'server-pending',
      'server-unknown',
      'server-notice',
      'server-new-live',
    ],
  );
  assert.equal(value.unread.has('public'), false);
});

test('edited messages fence older acknowledgements and update quotes without reviving removal', () => {
  const value = store();
  const original = entry('original');
  const quote = entry('reply', {
    replyTo: {
      messageId: original.messageId,
      participantId: 'alice',
      participantName: 'Alice',
      excerpt: original.content,
    },
  });
  value.receive(original);
  value.receive(quote);
  value.editMessage({
    ...original,
    revision: 2,
    editedAt: '2026-10-08T10:00:00Z',
    content: 'Corrected content',
  });
  value.receive({ ...original, revision: 1, content: 'Stale edit' }, true);
  assert.equal(value.messages[0].content, 'Corrected content');
  assert.equal(value.messages[1].replyTo.excerpt, 'Corrected content');
  value.removeMessage(original.messageId, '2026-10-08T11:00:00Z');
  value.editMessage({ ...original, revision: 3, content: 'Must not revive' });
  value.receive(original, true);
  assert.equal(value.messages[0].content, '');
  assert.equal(value.messages[1].replyTo.excerpt, 'Message removed');
});

test('an edit arriving before its original fences replay and remains conversation-private', () => {
  const value = store();
  const original = entry('first', { recipientId: 'local' });
  value.editMessage({ ...original, revision: 1, content: 'New text' });
  value.receive(original);
  assert.equal(value.messages[0].content, 'New text');
  assert.equal(
    value.editMessage(entry('someone-elses-pm', { recipientId: 'bob', revision: 1 })),
    false,
  );
});

test('saved PM drafts remain account and peer scoped, bounded, and tolerate unavailable storage', async () => {
  const { SavedPmDrafts } = await loadTypeScript('src/chat-store.ts');
  const data = new Map();
  const storage = {
    getItem: (key) => data.get(key) ?? null,
    setItem: (key, value) => data.set(key, value),
  };
  const drafts = new SavedPmDrafts(storage);
  drafts.save('alice', 'bob', 'Keep this draft');
  drafts.save('alice', 'carol', 'Other draft');
  assert.equal(new SavedPmDrafts(storage).get('alice', 'bob'), 'Keep this draft');
  assert.equal(drafts.get('carol', 'bob'), '');
  drafts.save('alice', 'bob', '');
  assert.equal(drafts.get('alice', 'carol'), 'Other draft');
  assert.equal(drafts.get('alice', 'bob'), '');
  for (let index = 0; index < 120; index++) drafts.save('alice', `peer-${index}`, 'x'.repeat(2000));
  assert.ok([...data.values()].every((text) => text.length < 70 * 1024));
  const denied = new SavedPmDrafts({
    getItem() {
      throw new Error('Denied');
    },
    setItem() {
      throw new Error('Denied');
    },
  });
  assert.doesNotThrow(() => denied.save('alice', 'bob', 'Typed'));
  assert.equal(denied.get('alice', 'bob'), '');
});
