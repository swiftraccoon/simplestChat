import { MAX_PASSWORD_BYTES, MAX_PASSWORD_CHARS, MIN_PASSWORD_CHARS } from './protocol';
import type {
  AccountProfile,
  CreateRoomRequest,
  PreferencePatch,
  RoomListItem,
  RoomSettingsPatch,
} from './protocol';
import {
  decodeAccountProfile,
  decodeAccountSessions,
  decodeServerCapabilities,
  decodeChatPreferences,
  decodeWebSocketTicket,
  decodeInviteRedemption,
  decodeMemberships,
  decodePublicProfile,
  decodeRecoveryKey,
  decodeCreatedRegistrationInvite,
  decodeRegistrationInvites,
  decodeCreatedRoomInvite,
  decodeRoomInvites,
  decodeRoomDirectory,
  decodeRoomListItem,
  decodePasskeyAction,
  decodePasskeySettings,
  type PasskeyOperation,
} from './api-validation';
import {
  decodeRoomSettings,
  decodeChatHistory,
  decodeChatEntry,
  decodeSocialData,
} from './protocol-validation';
import { decodeInbox, decodeChatRead, decodeInboxUnread } from './chat-history-validation';
import { decodeContacts, decodeSavedRooms } from './discovery-validation';
import {
  decodeNotificationPreferences,
  type NotificationPolicy,
  type ConversationNotificationPolicy,
} from './notification-validation';
import { type Decoder, isRecord } from './validation';
import {
  decodeAdministration,
  decodeAdministrationMembers,
  decodeAdministrationBans,
  decodeAdministrationReports,
  decodeAdministrationHistory,
  decodeAdministrationUpdated,
  decodeAdministrationUnban,
  decodeAdministrationResolution,
  decodeAdministrationLobby,
} from './room-administration-validation';
import type { Role } from './protocol';

/** Account password selection mirrors the server's NFC and Unicode-scalar policy. */
export function validatePassword(password: string, confirmation: string): void {
  const normalized = password.normalize('NFC');
  const length = Array.from(normalized).length;
  if (
    new TextEncoder().encode(password).length > MAX_PASSWORD_BYTES ||
    length < MIN_PASSWORD_CHARS ||
    length > MAX_PASSWORD_CHARS
  )
    throw new Error(
      `Use a password between ${MIN_PASSWORD_CHARS} and ${MAX_PASSWORD_CHARS} characters (maximum ${MAX_PASSWORD_BYTES} UTF-8 bytes)`,
    );
  // eslint-disable-next-line no-control-regex
  if (/[\u0000-\u001f\u007f-\u009f]/.test(normalized))
    throw new Error('Passwords cannot contain control characters');
  if (normalized !== confirmation.normalize('NFC')) throw new Error('New passwords do not match');
}

/** Small, text-safe controls shared by the community screens. */
export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  text?: string,
  className?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}

export function button(
  text: string,
  action: () => void,
  className = 'btn-secondary',
): HTMLButtonElement {
  const node = el('button', text, className);
  node.type = 'button';
  node.addEventListener('click', () => {
    action();
  });
  return node;
}

/** Own an asynchronous click while preserving the browser's user-gesture turn. */
export function asyncButton(
  text: string,
  action: () => Promise<void>,
  onError: (failure: unknown) => void,
  className = 'btn-secondary',
): HTMLButtonElement {
  return button(
    text,
    () => {
      try {
        action().catch(onError);
      } catch (failure) {
        onError(failure);
      }
    },
    className,
  );
}

export function field(label: string, input: HTMLElement): HTMLLabelElement {
  const wrapper = el('label', undefined, 'community-field');
  wrapper.append(el('span', label), input);
  return wrapper;
}

export function input(value = '', type = 'text', maxLength = 128): HTMLInputElement {
  const node = el('input');
  node.type = type;
  node.value = value;
  node.maxLength = maxLength;
  return node;
}

export type ModalDestination =
  { kind: 'inbox' } | { kind: 'conversation'; peerId: string; peerName: string };
export interface ModalNavigation {
  accountId: string;
  destination: ModalDestination;
}
type ModalNavigationHook = (
  dialog: HTMLDialogElement,
  canDismiss: () => boolean,
  navigation?: ModalNavigation,
) => (() => void) | void;
let modalNavigationHook: ModalNavigationHook | undefined;

/** The application owns history; individual dialogs keep their existing cleanup. */
export function setModalNavigation(hook: ModalNavigationHook): () => void {
  modalNavigationHook = hook;
  return () => {
    if (modalNavigationHook === hook) modalNavigationHook = undefined;
  };
}

export function modal(
  title: string,
  canDismiss: () => boolean = () => true,
  navigation?: ModalNavigation,
): {
  dialog: HTMLDialogElement;
  body: HTMLDivElement;
  error: HTMLParagraphElement;
  close: () => void;
} {
  const dialog = el('dialog', undefined, 'community-dialog');
  const heading = el('h2', title);
  heading.id = `dialog-${crypto.randomUUID()}`;
  dialog.setAttribute('aria-labelledby', heading.id);
  const header = el('div', undefined, 'community-dialog-header');
  const close = (): void => {
    if (canDismiss()) dialog.close();
  };
  const closeButton = button('Close', close);
  header.append(heading, closeButton);
  const body = el('div', undefined, 'community-dialog-body');
  const error = el('p', '', 'auth-error');
  error.hidden = true;
  error.setAttribute('role', 'alert');
  dialog.append(header, error, body);
  dialog.addEventListener('close', () => dialog.remove(), { once: true });
  dialog.addEventListener('cancel', (event) => {
    if (!canDismiss()) event.preventDefault();
  });
  dialog.addEventListener('click', (event) => {
    if (event.target !== dialog) return;
    const rect = dialog.getBoundingClientRect();
    if (
      event.clientX < rect.left ||
      event.clientX > rect.right ||
      event.clientY < rect.top ||
      event.clientY > rect.bottom
    )
      close();
  });
  document.body.append(dialog);
  dialog.showModal();
  const stopNavigation = modalNavigationHook?.(dialog, canDismiss, navigation);
  if (stopNavigation) dialog.addEventListener('close', stopNavigation, { once: true });
  return { dialog, body, error, close };
}

export async function busy(
  buttonNode: HTMLButtonElement,
  error: HTMLElement,
  work: () => Promise<void>,
): Promise<void> {
  if (buttonNode.disabled) return;
  buttonNode.disabled = true;
  let uncertain = false;
  error.hidden = true;
  try {
    await work();
  } catch (failure) {
    uncertain = failure instanceof ApiOutcomeUnknownError;
    error.textContent =
      failure instanceof Error ? failure.message : 'The action could not be completed';
    error.hidden = false;
  } finally {
    buttonNode.disabled = uncertain;
  }
}

/** Retain HTTP status without exposing unchecked response objects to UI callers. */
export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.status = status;

    this.name = 'ApiError';
  }
}

/** A transport failure cannot establish whether a server-side change committed. */
export class ApiOutcomeUnknownError extends Error {
  constructor() {
    super(
      'This change may have completed, but its response could not be confirmed. Reload and check the current state before trying again.',
    );
    this.name = 'ApiOutcomeUnknownError';
  }
}

/** Own the whole response, including its body, rather than only waiting for headers. */
async function boundedApi<T>(
  method: string,
  signal: AbortSignal | undefined,
  work: (signal: AbortSignal) => Promise<T>,
): Promise<T> {
  const controller = new AbortController();
  let timedOut = false;
  const deadline = Date.now() + 15_000;
  const cancel = (): void => controller.abort(signal?.reason);
  signal?.addEventListener('abort', cancel, { once: true });
  if (signal?.aborted) cancel();
  const timer = setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, 15_000);
  let aborted: () => void = () => {};
  let started = false;
  try {
    controller.signal.throwIfAborted();
    const stopped = new Promise<never>((_, reject) => {
      aborted = () => reject(new DOMException('Request cancelled', 'AbortError'));
      controller.signal.addEventListener('abort', aborted, { once: true });
    });
    started = true;
    const value = await Promise.race([work(controller.signal), stopped]);
    if (Date.now() >= deadline) {
      timedOut = true;
      throw new Error('Response deadline passed');
    }
    return value;
  } catch (error) {
    if (
      method !== 'GET' &&
      started &&
      !(
        error instanceof ApiError &&
        error.status >= 400 &&
        error.status < 500 &&
        error.status !== 408
      )
    )
      throw new ApiOutcomeUnknownError();
    if (timedOut) throw new Error('The server took too long to respond. Please try again.');
    throw error;
  } finally {
    clearTimeout(timer);
    controller.signal.removeEventListener('abort', aborted);
    signal?.removeEventListener('abort', cancel);
  }
}

async function apiResponse(
  path: string,
  token: string | null,
  method = 'GET',
  data?: unknown,
  signal?: AbortSignal,
  credentials: RequestCredentials = 'same-origin',
): Promise<Response> {
  const response = await fetch(path, {
    method,
    credentials,
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(data === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    ...(data === undefined ? {} : { body: JSON.stringify(data) }),
    ...(signal ? { signal } : {}),
  });
  if (!response.ok) {
    const raw = await response.text();
    let message = raw;
    try {
      const parsed: unknown = JSON.parse(raw);
      if (isRecord(parsed) && typeof parsed['error'] === 'string') message = parsed['error'];
    } catch {
      /* Text API error. */
    }
    throw new ApiError(
      message.slice(0, 400) || `Request failed (${response.status})`,
      response.status,
    );
  }
  return response;
}

async function apiJson<T>(
  decode: Decoder<T>,
  path: string,
  token: string | null,
  method = 'GET',
  data?: unknown,
  signal?: AbortSignal,
  credentials: RequestCredentials = 'same-origin',
): Promise<T> {
  return boundedApi(method, signal, async (ownedSignal) => {
    const response = await apiResponse(path, token, method, data, ownedSignal, credentials);
    try {
      if (response.status === 204) throw new Error('Expected JSON response');
      const value: unknown = await response.json();
      return decode(value);
    } catch {
      // JSON parse errors and decoded field failures must never display response
      // fragments, which can contain profile, account or infrastructure details.
      throw new Error('The server returned invalid data. Please try again.');
    }
  });
}

async function apiNoContent(
  path: string,
  token: string | null,
  method: 'POST' | 'PUT' | 'DELETE',
  data?: unknown,
  signal?: AbortSignal,
): Promise<void> {
  return boundedApi(method, signal, async (ownedSignal) => {
    const response = await apiResponse(path, token, method, data, ownedSignal);
    if (response.status !== 204)
      throw new Error('The server returned an unexpected response. Please try again.');
  });
}

/** Endpoint-owned contracts: callers cannot select an arbitrary response type or decoder. */
export const api = {
  administration: (token: string, id: string, signal: AbortSignal) =>
    apiJson(
      decodeAdministration,
      `/api/rooms/${encodeURIComponent(id)}/administration`,
      token,
      'GET',
      undefined,
      signal,
    ),
  administrationSettings: (
    token: string,
    id: string,
    patch: RoomSettingsPatch,
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeAdministration,
      `/api/rooms/${encodeURIComponent(id)}/administration/settings`,
      token,
      'PATCH',
      patch,
      signal,
    ),
  administrationMembers: (token: string, id: string, offset: number, signal: AbortSignal) =>
    apiJson(
      decodeAdministrationMembers,
      `/api/rooms/${encodeURIComponent(id)}/administration/members?offset=${offset}`,
      token,
      'GET',
      undefined,
      signal,
    ),
  administrationBans: (token: string, id: string, offset: number, signal: AbortSignal) =>
    apiJson(
      decodeAdministrationBans,
      `/api/rooms/${encodeURIComponent(id)}/administration/bans?offset=${offset}`,
      token,
      'GET',
      undefined,
      signal,
    ),
  administrationReports: (token: string, id: string, offset: number, signal: AbortSignal) =>
    apiJson(
      decodeAdministrationReports,
      `/api/rooms/${encodeURIComponent(id)}/administration/reports?offset=${offset}`,
      token,
      'GET',
      undefined,
      signal,
    ),
  administrationHistory: (token: string, id: string, offset: number, signal: AbortSignal) =>
    apiJson(
      decodeAdministrationHistory,
      `/api/rooms/${encodeURIComponent(id)}/administration/history?offset=${offset}`,
      token,
      'GET',
      undefined,
      signal,
    ),
  administrationRole: (
    token: string,
    id: string,
    target: string,
    role: Role,
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeAdministrationUpdated,
      `/api/rooms/${encodeURIComponent(id)}/administration/members/${encodeURIComponent(target)}`,
      token,
      'PUT',
      { role },
      signal,
    ),
  administrationUnban: (token: string, id: string, ban: string, signal: AbortSignal) =>
    apiJson(
      decodeAdministrationUnban,
      `/api/rooms/${encodeURIComponent(id)}/administration/bans/${encodeURIComponent(ban)}`,
      token,
      'DELETE',
      undefined,
      signal,
    ),
  administrationResolve: (
    token: string,
    id: string,
    report: string,
    status: 'resolved' | 'dismissed',
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeAdministrationResolution,
      `/api/rooms/${encodeURIComponent(id)}/administration/reports/${encodeURIComponent(report)}`,
      token,
      'PUT',
      { status },
      signal,
    ),
  administrationModerate: (
    token: string,
    id: string,
    request: {
      action: 'kick' | 'ban';
      targetParticipantId: string;
      reason?: string;
      durationSeconds?: number;
      reportId?: string;
    },
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeAdministrationUpdated,
      `/api/rooms/${encodeURIComponent(id)}/administration/moderation`,
      token,
      'POST',
      request,
      signal,
    ),
  administrationLobby: (token: string, id: string, offset: number, signal: AbortSignal) =>
    apiJson(
      decodeAdministrationLobby,
      `/api/rooms/${encodeURIComponent(id)}/administration/lobby?offset=${offset}`,
      token,
      'GET',
      undefined,
      signal,
    ),
  administrationLobbyAction: (
    token: string,
    id: string,
    target: string,
    action: 'admit' | 'deny',
    reason: string | undefined,
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeAdministrationUpdated,
      `/api/rooms/${encodeURIComponent(id)}/administration/lobby/${encodeURIComponent(target)}`,
      token,
      'POST',
      { action, reason },
      signal,
    ),
  administrationSanction: (
    token: string,
    id: string,
    request: {
      targetParticipantId: string;
      kind: 'camera' | 'chat';
      enabled: boolean;
      reason?: string;
    },
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeAdministrationUpdated,
      `/api/rooms/${encodeURIComponent(id)}/administration/sanctions`,
      token,
      'POST',
      request,
      signal,
    ),
  contacts: (token: string, signal: AbortSignal) =>
    apiJson(decodeContacts, '/api/auth/contacts', token, 'GET', undefined, signal),
  requestContact: (token: string, accountId: string, signal: AbortSignal) =>
    apiNoContent('/api/auth/contacts', token, 'POST', { accountId }, signal),
  acceptContact: (token: string, peer: string, signal: AbortSignal) =>
    apiNoContent(`/api/auth/contacts/${encodeURIComponent(peer)}`, token, 'PUT', undefined, signal),
  removeContact: (token: string, peer: string, signal: AbortSignal) =>
    apiNoContent(
      `/api/auth/contacts/${encodeURIComponent(peer)}`,
      token,
      'DELETE',
      undefined,
      signal,
    ),
  savedRooms: (token: string, signal: AbortSignal) =>
    apiJson(decodeSavedRooms, '/api/auth/saved-rooms', token, 'GET', undefined, signal),
  saveRoom: (token: string, roomId: string, favorite: boolean, signal: AbortSignal) =>
    apiNoContent(
      `/api/auth/saved-rooms/${encodeURIComponent(roomId)}`,
      token,
      'PUT',
      { favorite },
      signal,
    ),
  notificationPreferences: (token: string, signal: AbortSignal) =>
    apiJson(
      decodeNotificationPreferences,
      '/api/auth/notification-preferences',
      token,
      'GET',
      undefined,
      signal,
    ),
  saveNotificationPreferences: (token: string, policy: NotificationPolicy, signal: AbortSignal) =>
    apiJson(
      decodeNotificationPreferences,
      '/api/auth/notification-preferences',
      token,
      'PUT',
      policy,
      signal,
    ),
  patchNotificationPreferences: (
    token: string,
    patch: Partial<NotificationPolicy>,
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeNotificationPreferences,
      '/api/auth/notification-preferences',
      token,
      'PATCH',
      patch,
      signal,
    ),
  saveConversationNotifications: (
    token: string,
    peer: string,
    policy: ConversationNotificationPolicy,
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeNotificationPreferences,
      `/api/auth/notification-preferences/conversations/${encodeURIComponent(peer)}`,
      token,
      'PUT',
      policy,
      signal,
    ),
  pushStatus: (token: string, signal: AbortSignal) =>
    apiJson(
      (value: unknown) => {
        if (
          !isRecord(value) ||
          typeof value['enabled'] !== 'boolean' ||
          typeof value['publicKey'] !== 'string' ||
          !/^[A-Za-z0-9_-]{87}$/.test(value['publicKey'])
        )
          throw new Error('Invalid notification settings response');
        return { publicKey: value['publicKey'], enabled: value['enabled'] };
      },
      '/api/auth/push',
      token,
      'GET',
      undefined,
      signal,
    ),
  enablePush: (token: string, data: { endpoint: string }, signal: AbortSignal) =>
    apiNoContent('/api/auth/push', token, 'PUT', data, signal),
  disablePush: (token: string, signal: AbortSignal) =>
    apiNoContent('/api/auth/push', token, 'DELETE', undefined, signal),
  inboxUnread: (token: string, signal: AbortSignal) =>
    apiJson(decodeInboxUnread, '/api/auth/inbox/unread', token, 'GET', undefined, signal),
  editPrivateMessage: (
    token: string,
    peer: string,
    messageId: string,
    data: { content: string; expectedRevision: number },
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeChatEntry,
      `/api/auth/inbox/${encodeURIComponent(peer)}/messages/${encodeURIComponent(messageId)}`,
      token,
      'PUT',
      data,
      signal,
    ),
  inbox: (token: string, params: URLSearchParams, signal: AbortSignal) =>
    apiJson(decodeInbox, `/api/auth/inbox?${params}`, token, 'GET', undefined, signal),
  privateHistory: (token: string, peer: string, params: URLSearchParams, signal: AbortSignal) =>
    apiJson(
      decodeChatHistory,
      `/api/auth/inbox/${encodeURIComponent(peer)}/messages?${params}`,
      token,
      'GET',
      undefined,
      signal,
    ),
  sendPrivateMessage: (
    token: string,
    peer: string,
    data: {
      clientMessageId: string;
      content: string;
      attachmentIds?: string[];
      replyTo?: string;
      roomId?: string;
    },
    signal: AbortSignal,
  ) =>
    apiJson(
      decodeChatEntry,
      `/api/auth/inbox/${encodeURIComponent(peer)}/messages`,
      token,
      'POST',
      data,
      signal,
    ),
  setPrivateReaction: (
    token: string,
    peer: string,
    messageId: string,
    data: { emoji: string; present: boolean },
    signal: AbortSignal,
  ) =>
    apiJson(
      (value) => decodeSocialData('reactToMessage', value),
      `/api/auth/inbox/${encodeURIComponent(peer)}/messages/${encodeURIComponent(messageId)}/reactions`,
      token,
      'PUT',
      data,
      signal,
    ),
  readPrivateMessages: (token: string, peer: string, messageId: string, signal: AbortSignal) =>
    apiJson(
      decodeChatRead,
      `/api/auth/inbox/${encodeURIComponent(peer)}/read`,
      token,
      'PUT',
      { messageId },
      signal,
    ),
  websocketTicket: (token: string, signal: AbortSignal) =>
    apiJson(decodeWebSocketTicket, '/api/auth/ws-ticket', token, 'POST', {}, signal),
  capabilities: () =>
    apiJson(
      decodeServerCapabilities,
      '/api/capabilities',
      null,
      'GET',
      undefined,
      undefined,
      'omit',
    ),
  passkeySettings: (token: string, signal: AbortSignal) =>
    apiJson(decodePasskeySettings, '/api/auth/passkeys', token, 'GET', undefined, signal),
  passkeyAction: (
    token: string,
    data: { operation: PasskeyOperation; current_password?: string },
    signal: AbortSignal,
  ) => apiJson(decodePasskeyAction, '/api/auth/passkeys/start', token, 'POST', data, signal),
  passkeyAuthorize: (
    token: string,
    data: {
      ceremony_id: string;
      credential: ReturnType<typeof import('./auth').serializeCredential>;
    },
    signal: AbortSignal,
  ) => apiJson(decodePasskeyAction, '/api/auth/passkeys/authorize', token, 'POST', data, signal),
  passkeyEnroll: (
    token: string,
    data: {
      ceremony_id: string;
      credential: ReturnType<typeof import('./auth').serializeCredential>;
    },
    signal: AbortSignal,
  ) => apiJson(decodePasskeyAction, '/api/auth/passkeys/enroll', token, 'POST', data, signal),
  // Public reads carry no credential: nothing needs one, and fewer requests hold them.
  publicProfile: (id: string) =>
    apiJson(
      decodePublicProfile,
      `/api/auth/profiles/${encodeURIComponent(id)}`,
      null,
      'GET',
      undefined,
      undefined,
      'omit',
    ),
  accountProfile: (token: string | null) =>
    apiJson(decodeAccountProfile, '/api/auth/profile', token),
  accountSessions: (token: string | null, signal?: AbortSignal) =>
    apiJson(decodeAccountSessions, '/api/auth/sessions', token, 'GET', undefined, signal),
  revokeSession: (token: string | null, id: string, signal?: AbortSignal) =>
    apiNoContent(
      `/api/auth/sessions/${encodeURIComponent(id)}`,
      token,
      'DELETE',
      undefined,
      signal,
    ),
  revokeOtherSessions: (token: string | null, signal?: AbortSignal) =>
    apiNoContent('/api/auth/sessions/others', token, 'DELETE', undefined, signal),
  updateProfile: (
    token: string | null,
    data: Pick<AccountProfile, 'display_name' | 'bio' | 'avatar_url' | 'profile_style'>,
    signal?: AbortSignal,
  ) => apiJson(decodeAccountProfile, '/api/auth/profile', token, 'PATCH', data, signal),
  changePassword: (
    token: string | null,
    data: { current_password: string; new_password: string },
    signal?: AbortSignal,
  ) => apiNoContent('/api/auth/password', token, 'POST', data, signal),
  accountPreferences: (token: string | null, signal?: AbortSignal) =>
    apiJson(decodeChatPreferences, '/api/auth/preferences', token, 'GET', undefined, signal),
  registrationInvites: (token: string | null) =>
    apiJson(decodeRegistrationInvites, '/api/auth/invites', token),
  createRegistrationInvite: (token: string | null) =>
    apiJson(decodeCreatedRegistrationInvite, '/api/auth/invites', token, 'POST'),
  revokeRegistrationInvite: (token: string | null, id: string) =>
    apiNoContent(`/api/auth/invites/${encodeURIComponent(id)}`, token, 'DELETE'),
  memberships: (token: string | null, after?: string) =>
    apiJson(
      decodeMemberships,
      `/api/rooms/memberships${after ? `?after=${encodeURIComponent(after)}` : ''}`,
      token,
    ),
  roomInvites: (token: string | null, id: string) =>
    apiJson(decodeRoomInvites, `/api/rooms/${encodeURIComponent(id)}/invites`, token),
  createRoomInvite: (
    token: string | null,
    id: string,
    data: { role: Role; uses: number; days: number },
  ) =>
    apiJson(
      decodeCreatedRoomInvite,
      `/api/rooms/${encodeURIComponent(id)}/invites`,
      token,
      'POST',
      data,
    ),
  revokeRoomInvite: (token: string | null, id: string, inviteId: string) =>
    apiNoContent(
      `/api/rooms/${encodeURIComponent(id)}/invites/${encodeURIComponent(inviteId)}`,
      token,
      'DELETE',
    ),
  previewInvite: (token: string | null, code: string) =>
    apiJson(decodeInviteRedemption, '/api/rooms/invites/preview', token, 'POST', { code }),
  redeemInvite: (token: string | null, code: string) =>
    apiJson(decodeInviteRedemption, '/api/rooms/invites/redeem', token, 'POST', { code }),
  updatePreferences: (token: string | null, data: PreferencePatch, signal?: AbortSignal) =>
    apiJson(decodeChatPreferences, '/api/auth/preferences', token, 'PATCH', data, signal),
  recoveryKey: (token: string | null, data: { current_password: string }) =>
    apiJson(decodeRecoveryKey, '/api/auth/recovery/key', token, 'POST', data),
  redeemRecovery: (data: { email: string; recovery_key: string; new_password: string }) =>
    apiNoContent('/api/auth/recovery/redeem', null, 'POST', data),
  rooms: (query: URLSearchParams, signal?: AbortSignal) =>
    apiJson(decodeRoomDirectory, `/api/rooms?${query}`, null, 'GET', undefined, signal, 'omit'),
  ownRooms: (token: string | null) => apiJson(decodeRoomDirectory, '/api/rooms/mine', token),
  createRoom: (token: string | null, data: CreateRoomRequest) =>
    apiJson(decodeRoomSettings, '/api/rooms', token, 'POST', data),
  updateRoomIdentity: (
    id: string,
    token: string | null,
    data: {
      display_name: string;
      topic: string | null;
      description: string;
      /** Omitted keeps the current image, null removes it, a data URL replaces it. */
      image_url?: string | null | undefined;
      name_style: RoomListItem['name_style'];
      topic_style: RoomListItem['topic_style'];
    },
  ) =>
    apiJson(
      decodeRoomListItem,
      `/api/rooms/${encodeURIComponent(id)}/identity`,
      token,
      'PATCH',
      data,
    ),
  deleteRoom: (id: string, token: string | null) =>
    apiNoContent(`/api/rooms/${encodeURIComponent(id)}`, token, 'DELETE'),
};

/** Decode and re-encode raster uploads; never inject file markup or remote URLs. */
export async function rasterUpload(file: File, width = 192, height = 192): Promise<string> {
  if (
    !['image/png', 'image/jpeg', 'image/webp'].includes(file.type) ||
    file.size > 8 * 1024 * 1024
  ) {
    throw new Error('Choose a PNG, JPEG or WebP image smaller than 8 MB');
  }
  const url = URL.createObjectURL(file);
  try {
    const image = new Image();
    image.src = url;
    await image.decode();
    if (!image.naturalWidth || !image.naturalHeight) throw new Error('Image could not be decoded');
    const canvas = el('canvas');
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext('2d');
    if (!context) throw new Error('Image processing is unavailable');
    const scale = Math.max(width / image.naturalWidth, height / image.naturalHeight);
    const sourceWidth = width / scale;
    const sourceHeight = height / scale;
    context.drawImage(
      image,
      (image.naturalWidth - sourceWidth) / 2,
      (image.naturalHeight - sourceHeight) / 2,
      sourceWidth,
      sourceHeight,
      0,
      0,
      width,
      height,
    );
    const result = canvas.toDataURL('image/jpeg', 0.8);
    if (result.length > 170_000)
      throw new Error('Image is too detailed; please choose a smaller image');
    return result;
  } finally {
    URL.revokeObjectURL(url);
  }
}

/** A raster the browser may show: an inline PNG, JPEG or WebP data URL, or the
 * same-origin room image path a listing names by its content digest. */
export function safeRasterUrl(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    ((value.length <= 180_000 &&
      /^data:image\/(?:png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(value)) ||
      /^\/api\/rooms\/[A-Za-z0-9_-]{1,128}\/image\?v=[0-9a-f]{16}$/.test(value))
  );
}
