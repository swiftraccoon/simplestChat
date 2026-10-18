import type { RoomAdministrationView, RoomAdministrationLobbyPage } from './room-administration';
import { decodeRoomSettings, decodeSocialData } from './protocol-validation';
import { boolean, choice, invalid, list, object, text } from './validation';

export const decodeAdministration = object<RoomAdministrationView>({
  role: choice('moderator', 'admin', 'owner'),
  settings: decodeRoomSettings,
});
const lobby = object<RoomAdministrationLobbyPage>({
  lobby: list(
    object({
      participantId: text,
      displayName: text,
      authenticated: boolean,
      role: choice('guest', 'user', 'member', 'moderator', 'admin', 'owner'),
    }),
  ),
  hasMore: boolean,
});
export const decodeAdministrationLobby = (value: unknown): RoomAdministrationLobbyPage => {
  const page = lobby(value);
  return page.lobby.length <= 100 ? page : invalid();
};
export const decodeAdministrationMembers = (value: unknown) => {
  const page = decodeSocialData('listRoomMembers', value);
  return page.members.length <= 100 ? page : invalid();
};
export const decodeAdministrationBans = (value: unknown) => {
  const page = decodeSocialData('listRoomBans', value);
  return page.bans.length <= 100 ? page : invalid();
};
export const decodeAdministrationReports = (value: unknown) => {
  const page = decodeSocialData('listRoomReports', value);
  return page.reports.length <= 100 ? page : invalid();
};
export const decodeAdministrationHistory = (value: unknown) => {
  const page = decodeSocialData('listModerationEvents', value);
  return page.events.length <= 100 ? page : invalid();
};
export const decodeAdministrationUpdated = (value: unknown) =>
  decodeSocialData('setMemberRole', value);
export const decodeAdministrationUnban = (value: unknown) =>
  decodeSocialData('removeRoomBan', value);
export const decodeAdministrationResolution = (value: unknown) =>
  decodeSocialData('resolveRoomReport', value);
