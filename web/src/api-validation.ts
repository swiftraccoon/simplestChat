import type { AccountProfile, PublicProfile, RoomListItem } from './protocol';
import { boolean, integer, list, nullable, object, text } from './validation';

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
  participant_count: integer(),
  password_protected: boolean,
  moderated: boolean,
  broadcaster_count: integer(),
  description: text,
  image_url: nullable(text),
  secret: boolean,
});
export const decodeRoomDirectory = list(decodeRoomListItem);
