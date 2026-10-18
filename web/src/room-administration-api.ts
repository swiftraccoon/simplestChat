import { api } from './ui';
import type { RoomAdministrationApi } from './room-administration';

export const roomAdministrationApi: RoomAdministrationApi = {
  overview: api.administration,
  settings: api.administrationSettings,
  members: api.administrationMembers,
  bans: api.administrationBans,
  reports: api.administrationReports,
  history: api.administrationHistory,
  setRole: async (...args) => {
    await api.administrationRole(...args);
  },
  removeBan: async (...args) => {
    await api.administrationUnban(...args);
  },
  resolveReport: async (...args) => {
    await api.administrationResolve(...args);
  },
  moderate: async (...args) => {
    await api.administrationModerate(...args);
  },
  lobby: api.administrationLobby,
  lobbyAction: async (...args) => {
    await api.administrationLobbyAction(...args);
  },
  sanction: async (...args) => {
    await api.administrationSanction(...args);
  },
};
