import type { AccountPreferences } from '../account-preferences';
import type { RoomClient } from '../room';
import type { TelemetryHandler } from '../telemetry-types';

export type SocialChatOptions = {
  telemetry?: TelemetryHandler;
  getRoom: () => RoomClient | null;
  getViewerKey: () => string;
  /** The account's bearer token, when the viewer has one: its preferences then follow it. */
  getToken?: () => string | null;
  accountPreferences?: AccountPreferences;
  /** Account PMs share the durable inbox surface regardless of room presence. */
  openAccountConversation?: (id: string, name: string) => void;
  notify: (message: string) => void;
  bindParticipantName: (anchor: HTMLButtonElement, id: string, name: string) => void;
  allowsNotification?: (kind: 'private' | 'mention' | 'room', peerId?: string) => boolean;
  mountConversationNotifications?: (
    container: HTMLElement,
    peerId: string,
    current: () => boolean,
  ) => void;
  /** Opens the settings sheet at a section; the chat options button uses it. */
  openSettings?: (section: string, opener: HTMLElement | null) => void;
  /** Account notification rules and app alerts, appended to the Notifications screen. */
  mountAccountNotifications?: (container: HTMLElement, current: () => boolean) => void;
};
