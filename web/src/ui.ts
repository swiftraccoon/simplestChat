import type { CreateRoomRequest } from './protocol';
import {
  type ChatPreferences,
  decodeAccountProfile,
  decodeServerCapabilities,
  decodeChatPreferences,
  decodeInviteRedemption,
  decodeMemberships,
  decodePublicProfile,
  decodeRecoveryKey,
  decodeRegistrationInvite,
  decodeRegistrationInvites,
  decodeRoomInvite,
  decodeRoomInvites,
  decodeRoomDirectory,
  decodeRoomListItem,
  decodePasskeyAction,
  decodePasskeySettings,
  type PasskeyOperation,
} from './api-validation';
import { decodeRoomSettings } from './protocol-validation';
import { type Decoder, isRecord } from './validation';

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

export function modal(
  title: string,
  canDismiss: () => boolean = () => true,
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
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
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
  method: 'POST' | 'DELETE',
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
  updateProfile: (
    token: string | null,
    data: { display_name: string; bio: string; avatar_url: string | null },
    signal?: AbortSignal,
  ) => apiJson(decodeAccountProfile, '/api/auth/profile', token, 'PATCH', data, signal),
  changePassword: (
    token: string | null,
    data: { current_password: string; new_password: string },
    signal?: AbortSignal,
  ) => apiNoContent('/api/auth/password', token, 'POST', data, signal),
  accountPreferences: (token: string | null) =>
    apiJson(decodeChatPreferences, '/api/auth/preferences', token),
  registrationInvites: (token: string | null) =>
    apiJson(decodeRegistrationInvites, '/api/auth/invites', token),
  createRegistrationInvite: (token: string | null) =>
    apiJson(decodeRegistrationInvite, '/api/auth/invites', token, 'POST'),
  revokeRegistrationInvite: (token: string | null, code: string) =>
    apiNoContent(`/api/auth/invites/${encodeURIComponent(code)}`, token, 'DELETE'),
  memberships: (token: string | null, after?: string) =>
    apiJson(
      decodeMemberships,
      `/api/rooms/memberships?paginated=true${after ? `&after=${encodeURIComponent(after)}` : ''}`,
      token,
    ),
  roomInvites: (token: string | null, id: string) =>
    apiJson(decodeRoomInvites, `/api/rooms/${encodeURIComponent(id)}/invites`, token),
  createRoomInvite: (
    token: string | null,
    id: string,
    data: { role: number; uses: number; days: number },
  ) =>
    apiJson(decodeRoomInvite, `/api/rooms/${encodeURIComponent(id)}/invites`, token, 'POST', data),
  revokeRoomInvite: (token: string | null, id: string, code: string) =>
    apiNoContent(
      `/api/rooms/${encodeURIComponent(id)}/invites/${encodeURIComponent(code)}`,
      token,
      'DELETE',
    ),
  redeemInvite: (token: string | null, code: string) =>
    apiJson(
      decodeInviteRedemption,
      `/api/rooms/invites/${encodeURIComponent(code)}`,
      token,
      'POST',
    ),
  updatePreferences: (token: string | null, data: ChatPreferences) =>
    apiJson(decodeChatPreferences, '/api/auth/preferences', token, 'PUT', data),
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
      image_url: string | null;
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

export function safeRasterUrl(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length <= 180_000 &&
    /^data:image\/(?:png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(value)
  );
}
