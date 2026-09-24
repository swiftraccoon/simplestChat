import type { AccountProfile, PublicProfile, RoomListItem } from './protocol';
import { boolean, integer, list, nullable, object, text, record, invalid } from './validation';

export interface PasskeySettings {
  password_enabled: boolean;
  recovery_enabled: boolean;
  passkeys: { id: string; created_at: string }[];
  maximum: number;
}
export type PasskeyOperation =
  { action: 'add' } | { action: 'remove'; id: string } | { action: 'recovery_key' };
export type PasskeyActionResponse =
  | { kind: 'authenticate' | 'register'; ceremony_id: string; options: Record<string, unknown> }
  | { kind: 'recovery_key'; recovery_key: string }
  | { kind: 'removed' | 'added' };

function exact(value: unknown, keys: string[]): Record<string, unknown> {
  const result = record(value);
  if (Object.keys(result).some((key) => !keys.includes(key))) invalid();
  return result;
}
function boundedText(value: unknown, maximum: number): string {
  const result = text(value);
  return result.length > 0 && result.length <= maximum ? result : invalid();
}
function recordId(value: unknown): string {
  const result = text(value);
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(result)
    ? result
    : invalid();
}
export function decodePasskeySettings(value: unknown): PasskeySettings {
  const result = exact(value, ['password_enabled', 'recovery_enabled', 'passkeys', 'maximum']);
  const maximum = integer(10, 1)(result['maximum']);
  const passkeys = list((entry: unknown) => {
    const key = exact(entry, ['id', 'created_at']);
    const created = boundedText(key['created_at'], 40);
    if (
      !/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/.test(created) ||
      !Number.isFinite(Date.parse(created))
    )
      invalid();
    return { id: recordId(key['id']), created_at: created };
  })(result['passkeys']);
  if (passkeys.length > maximum || new Set(passkeys.map((key) => key.id)).size !== passkeys.length)
    invalid();
  return {
    password_enabled: boolean(result['password_enabled']),
    recovery_enabled: boolean(result['recovery_enabled']),
    passkeys,
    maximum,
  };
}
export function decodePasskeyAction(value: unknown): PasskeyActionResponse {
  const result = record(value);
  const kind = result['kind'];
  if (kind === 'added' || kind === 'removed') {
    exact(value, ['kind']);
    return { kind };
  }
  if (kind === 'recovery_key') {
    exact(value, ['kind', 'recovery_key']);
    return { kind, recovery_key: boundedText(result['recovery_key'], 256) };
  }
  if (kind === 'authenticate' || kind === 'register') {
    exact(value, ['kind', 'ceremony_id', 'options']);
    const options = exact(result['options'], ['publicKey', 'mediation']);
    record(options['publicKey']);
    return { kind, ceremony_id: boundedText(result['ceremony_id'], 256), options };
  }
  return invalid();
}

const profileFields = {
  id: text,
  display_name: text,
  avatar_url: nullable(text),
  bio: text,
};
export const decodePublicProfile = object<PublicProfile>(profileFields);
export const decodeAccountProfile = object<AccountProfile>({
  ...profileFields,
  email: text,
  recovery_enabled: boolean,
});
export const decodeRecoveryKey = object<{ recovery_key: string }>({ recovery_key: text });

/** These fields are all serialized by src/room/api.rs, including nullable fields. */
export const decodeRoomListItem = object<RoomListItem>({
  id: text,
  display_name: text,
  topic: nullable(text),
  participant_count: nullable(integer()),
  password_protected: boolean,
  moderated: boolean,
  broadcaster_count: nullable(integer()),
  description: text,
  image_url: nullable(text),
  secret: boolean,
});
export const decodeRoomDirectory = list(decodeRoomListItem);
