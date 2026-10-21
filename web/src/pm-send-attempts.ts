import type { ChatAttachment, ChatReplyRef } from './protocol';
import { decodeAttachment } from './protocol-validation';
import { integer, invalid, record, text } from './validation';

export const PM_SEND_LOCK = 'simplestchat.pm-send';
export const PM_SEND_STORAGE_KEY = 'simplestchat.pm-attempts';
const MAX_BYTES = 64 * 1024;
const MAX_RECORDS = 100;
const MAX_AGE = 24 * 60 * 60 * 1000;

export interface PmSendPayload {
  id: string;
  content: string;
  attachments: ChatAttachment[];
  replyTo?: ChatReplyRef;
}
export interface PmSendAttempt extends PmSendPayload {
  createdAt: number;
}
export type PmSendState =
  | { kind: 'empty' | 'unavailable' | 'full' }
  | { kind: 'pending' | 'expired' | 'conflict'; attempt: PmSendAttempt };
type Stored = [string, string, PmSendAttempt];

function bytes(value: string): number {
  return new TextEncoder().encode(value).length;
}
function identity(value: unknown): string {
  const id = text(value);
  return /^[A-Za-z0-9_-]{1,128}$/.test(id) ? id : invalid();
}
function decode(value: unknown): PmSendAttempt {
  const source = record(value);
  const id = text(source['id']);
  const content = text(source['content']);
  const files = source['attachments'];
  if (
    Object.keys(source).length !== (source['replyTo'] === undefined ? 4 : 5) ||
    !/^[A-Za-z0-9_-]{1,64}$/.test(id) ||
    content.length > 2000 ||
    bytes(content) > 4096 ||
    !Array.isArray(files) ||
    files.length > 4
  )
    invalid();
  const attachments = files.map((file: unknown) => {
    if (Object.keys(record(file)).length !== 4) invalid();
    return decodeAttachment(file);
  });
  if (
    (!content.trim() && !attachments.length) ||
    new Set(attachments.map((file) => file.id)).size !== attachments.length
  )
    invalid();
  let replyTo: ChatReplyRef | undefined;
  if (source['replyTo'] !== undefined) {
    const reply = record(source['replyTo']);
    const messageId = text(reply['messageId']);
    const participantId = identity(reply['participantId']);
    const participantName = text(reply['participantName']);
    const excerpt = text(reply['excerpt']);
    if (
      Object.keys(reply).length !== 4 ||
      !/^[A-Za-z0-9_-]{1,64}$/.test(messageId) ||
      !participantName ||
      bytes(participantName) > 512 ||
      [...excerpt].length > 141 ||
      bytes(excerpt) > 564
    )
      invalid();
    replyTo = { messageId, participantId, participantName, excerpt };
  }
  return {
    id,
    content,
    attachments,
    ...(replyTo && { replyTo }),
    createdAt: integer()(source['createdAt']),
  };
}

/** Caller holds PM_SEND_LOCK across begin/POST/clear. localStorage is not a CAS.
 * Read-only views may inspect without a lock. Unresolved entries never evict. */
export class PmSendAttempts {
  private readonly storage: Pick<Storage, 'getItem' | 'setItem'>;
  private readonly now: () => number;

  constructor(storage: Pick<Storage, 'getItem' | 'setItem'>, now: () => number = Date.now) {
    this.storage = storage;
    this.now = now;
  }

  private rows(): Stored[] {
    const raw = this.storage.getItem(PM_SEND_STORAGE_KEY) ?? '[]';
    if (raw.length > MAX_BYTES || bytes(raw) > MAX_BYTES) invalid();
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value) || value.length > MAX_RECORDS) invalid();
    const seen = new Set<string>();
    return value.map((row: unknown): Stored => {
      if (!Array.isArray(row) || row.length !== 3) invalid();
      const account = identity(row[0]);
      const peer = identity(row[1]);
      const key = `${account}:${peer}`;
      if (account === peer || seen.has(key)) invalid();
      seen.add(key);
      return [account, peer, decode(row[2])];
    });
  }

  private state(attempt: PmSendAttempt): PmSendState {
    const age = this.now() - attempt.createdAt;
    return {
      kind: !Number.isFinite(age) || age < 0 || age >= MAX_AGE ? 'expired' : 'pending',
      attempt,
    };
  }

  private write(rows: Stored[]): boolean {
    const raw = JSON.stringify(rows);
    if (bytes(raw) > MAX_BYTES) return false;
    this.storage.setItem(PM_SEND_STORAGE_KEY, raw);
    return this.storage.getItem(PM_SEND_STORAGE_KEY) === raw;
  }

  read(account: string, peer: string): PmSendState {
    try {
      identity(account);
      identity(peer);
      const row = this.rows().find(([own, other]) => own === account && other === peer);
      return row ? this.state(row[2]) : { kind: 'empty' };
    } catch {
      return { kind: 'unavailable' };
    }
  }

  begin(account: string, peer: string, payload: PmSendPayload): PmSendState {
    try {
      identity(account);
      identity(peer);
      if (account === peer) invalid();
      const rows = this.rows();
      const attempt = decode({ ...payload, createdAt: this.now() });
      const existing = rows.find(([own, other]) => own === account && other === peer)?.[2];
      if (existing) {
        const state = this.state(existing);
        if (
          state.kind === 'pending' &&
          JSON.stringify({ ...existing, createdAt: attempt.createdAt }) !== JSON.stringify(attempt)
        )
          return { kind: 'conflict', attempt: existing };
        return state;
      }
      if (rows.length >= MAX_RECORDS) return { kind: 'full' };
      rows.push([account, peer, attempt]);
      if (bytes(JSON.stringify(rows)) > MAX_BYTES) return { kind: 'full' };
      return this.write(rows) ? this.state(attempt) : { kind: 'unavailable' };
    } catch {
      return { kind: 'unavailable' };
    }
  }

  /** Use after an authoritative receipt or explicit dismissal, never on close. */
  clear(account: string, peer: string, id: string): boolean {
    try {
      const rows = this.rows();
      const remaining = rows.filter(
        ([own, other, attempt]) => own !== account || other !== peer || attempt.id !== id,
      );
      return remaining.length < rows.length && this.write(remaining);
    } catch {
      return false;
    }
  }
}
