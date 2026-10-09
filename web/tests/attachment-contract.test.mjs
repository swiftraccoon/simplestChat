import assert from 'node:assert/strict';
import test from 'node:test';
import { loadContractModules } from './source-loader.mjs';
const { decodeAttachment, decodeChatEntry, decodeSocialData } = (await loadContractModules())[
  './protocol-validation'
];
const attachment = {
  id: '11111111-1111-4111-8111-111111111111',
  name: 'Photo.png',
  contentType: 'image/png',
  size: 123,
};
const message = {
  messageId: 'message',
  clientMessageId: 'client',
  participantId: 'peer',
  participantName: 'Peer',
  content: '',
  sentAt: '2026-10-09T12:00:00Z',
  revision: 0,
};

test('attachment wire metadata is bounded, raster-or-download only, and strips extra fields', () => {
  assert.deepEqual(decodeAttachment({ ...attachment, private: 'discard' }), attachment);
  for (const change of [
    { id: '../file' },
    { name: '' },
    { name: 'a'.repeat(121) },
    { name: 'bad\nname' },
    { size: 0 },
    { size: 5242881 },
    { size: 1.5 },
    { contentType: 'image/svg+xml' },
    { contentType: 'text/html' },
  ]) {
    assert.throws(() => decodeAttachment({ ...attachment, ...change }));
  }
  assert.equal(
    decodeAttachment({ ...attachment, contentType: 'application/octet-stream', size: 5242880 })
      .size,
    5242880,
  );
});

test('chat decoding rejects duplicate or oversized attachment sets', () => {
  assert.deepEqual(decodeChatEntry({ ...message, attachments: [attachment] }).attachments, [
    attachment,
  ]);
  assert.throws(() => decodeChatEntry({ ...message, attachments: [attachment, attachment] }));
  const many = Array.from({ length: 5 }, (_, index) => ({
    ...attachment,
    id: `11111111-1111-4111-8111-11111111111${index}`,
  }));
  assert.throws(() => decodeChatEntry({ ...message, attachments: many }));
});

test('attachment grants require bounded opaque tokens and explicit valid expiry timestamps', () => {
  const grant = { token: 'signed.fixture.token', expiresAt: '2026-10-09T12:05:00Z' };
  assert.deepEqual(decodeSocialData('getAttachmentAccess', grant), grant);
  for (const change of [
    { token: '' },
    { token: 'x'.repeat(4097) },
    { token: 'bad\r\nheader' },
    { expiresAt: 'tomorrow' },
  ]) {
    assert.throws(() => decodeSocialData('getAttachmentAccess', { ...grant, ...change }));
  }
});
