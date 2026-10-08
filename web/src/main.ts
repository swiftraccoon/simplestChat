import './style.css';
import { SignalingClient } from './signaling';
import {
  RoomClient,
  RoomPasswordRequiredError,
  type Participant,
  type ConnectionQuality,
} from './room';
import * as icons from './icons';
import { AuthManager, SessionOutcomeUnknownError } from './auth';
import { AuthDialogFlow, type AuthDialogAttempt } from './auth-dialog';
import { AccountSessionSync } from './account-session-sync';
import { RoomNavigation } from './room-navigation';
import type { ScreenShareResult } from './media';
import { ClientTelemetry } from './telemetry';
import { CallOutcomeTelemetry, MediaTelemetry, observeFirstVideoFrame } from './media-telemetry';
import { MediaControls, mediaErrorMessage } from './media-controls';
import { MediaLifecycle } from './media-lifecycle';
import { SocialChat } from './social-chat';
import { CommunityUI } from './community-ui';
import { ParticipantHovercard } from './participant-hovercard';
import { applyAppearance } from './appearance';
import {
  api,
  ApiError,
  ApiOutcomeUnknownError,
  button,
  el,
  modal,
  safeRasterUrl,
  validatePassword,
} from './ui';
import { configureSettingsDialog } from './settings-dialog';
import { avatarColors, chatColor } from './avatar-colors';
import { spatialLayerForRenderedWidth } from './layer-cap';
import { observeVideoLayout } from './video-layout';
import './community.css';
import './participant-hovercard.css';
import type { CreateRoomRequest, RoomSettingsPatch } from './protocol';
import type { ServerCapabilities } from './api-validation';

declare const __APP_REVISION__: string;
const telemetry = new ClientTelemetry(__APP_REVISION__);
window.addEventListener('error', () => telemetry.record({ name: 'js_error', outcome: 'error' }));
window.addEventListener('unhandledrejection', () =>
  telemetry.record({ name: 'unhandled_rejection', outcome: 'error' }),
);

// --- DOM refs ---
const connectionStatus = document.getElementById('connection-status')!;
const joinScreen = document.getElementById('join-screen')!;
const roomScreen = document.getElementById('room-screen')!;
const roomLabel = document.getElementById('room-label')!;
const roomTopic = document.getElementById('room-topic')!;
const homeLink = document.getElementById('home-link')!;
const nameInput = document.getElementById('name-input') as HTMLInputElement;
const roomInput = document.getElementById('room-input') as HTMLInputElement;
const joinBtn = document.getElementById('join-btn') as HTMLButtonElement;
const videoGrid = document.getElementById('video-grid')!;
observeVideoLayout(videoGrid);
const participantList = document.getElementById('participant-list')!;
const chatMessages = document.getElementById('chat-messages')!;
const chatInput = document.getElementById('chat-input') as HTMLTextAreaElement;
const chatSendBtn = document.getElementById('chat-send-btn')!;
const micBtn = document.getElementById('mic-btn')!;
const camBtn = document.getElementById('cam-btn')!;
const screenBtn = document.getElementById('screen-btn')!;
const settingsBtn = document.getElementById('settings-btn')!;
const leaveBtn = document.getElementById('leave-btn')!;
const qualityIndicator = document.getElementById('quality-indicator')!;
const scrollBottomBtn = document.getElementById('scroll-bottom-btn')!;
const unreadBadge = document.getElementById('unread-badge')!;
const handBtn = document.getElementById('hand-btn')!;
const roomSettingsBtn = document.getElementById('room-settings-btn')!;
const roomTools = document.getElementById('room-tools')!;
const rosterToggleBtn = document.getElementById('toggle-roster') as HTMLButtonElement;
const chatToggleBtn = document.getElementById('toggle-chat') as HTMLButtonElement;

// Lobby screen
const lobbyScreen = document.getElementById('lobby-screen')!;
const lobbyRoomName = document.getElementById('lobby-room-name')!;
const lobbyTopic = document.getElementById('lobby-topic')!;
const lobbyCount = document.getElementById('lobby-count')!;
const lobbyCancelBtn = document.getElementById('lobby-cancel-btn')!;

// Lobby management panel
const lobbyTab = document.getElementById('lobby-tab') as HTMLButtonElement;
const lobbyListEl = document.getElementById('lobby-list')!;
const lobbyEmpty = document.getElementById('lobby-empty')!;

// Auth UI
const authBarGuest = document.getElementById('auth-bar-guest')!;
const authBarUser = document.getElementById('auth-bar-user')!;
const authDisplayName = document.getElementById('auth-display-name')!;
const signInBtn = document.getElementById('sign-in-btn')!;
const logoutBtn = document.getElementById('logout-btn')!;
const roomBrowser = document.getElementById('room-browser')!;
const roomSearchInput = document.getElementById('room-search-input') as HTMLInputElement;
const roomList = document.getElementById('room-list')!;
const roomLoadMore = document.getElementById('room-load-more') as HTMLButtonElement;
const createRoomBtn = document.getElementById('create-room-btn')!;
const serverMode = document.getElementById('server-mode')!;
const capabilitiesRetry = document.getElementById('server-features-retry') as HTMLButtonElement;
const joinFormDivider = document.getElementById('join-form-divider')!;

// Login modal
const loginModal = document.getElementById('login-modal') as HTMLDialogElement;
const loginClose = document.getElementById('login-close')!;
const loginEmail = document.getElementById('login-email') as HTMLInputElement;
const loginPassword = document.getElementById('login-password') as HTMLInputElement;
const loginSubmit = document.getElementById('login-submit') as HTMLButtonElement;
const loginError = document.getElementById('login-error')!;
const loginPasskeyBtn = document.getElementById('login-passkey-btn') as HTMLButtonElement;
const loginToRegister = document.getElementById('login-to-register')!;

// Register modal
const registerModal = document.getElementById('register-modal') as HTMLDialogElement;
const registerClose = document.getElementById('register-close')!;
const registerEmail = document.getElementById('register-email') as HTMLInputElement;
const registerName = document.getElementById('register-name') as HTMLInputElement;
const registerPassword = document.getElementById('register-password') as HTMLInputElement;
const registerConfirm = document.getElementById('register-confirm') as HTMLInputElement;
const registerInvite = document.getElementById('register-invite') as HTMLInputElement;
const registerSubmit = document.getElementById('register-submit') as HTMLButtonElement;
const registerError = document.getElementById('register-error')!;
const registerPasskeyBtn = document.getElementById('register-passkey-btn') as HTMLButtonElement;
const registerToLogin = document.getElementById('register-to-login')!;

// Create room modal
const createRoomModal = document.getElementById('create-room-modal') as HTMLDialogElement;
let createRoomAttempt = 0;
let createRoomMutation = 0;
const createRoomClose = document.getElementById('create-room-close')!;
const crId = document.getElementById('cr-id') as HTMLInputElement;
const crName = document.getElementById('cr-name') as HTMLInputElement;
const crTopic = document.getElementById('cr-topic') as HTMLInputElement;
const crPassword = document.getElementById('cr-password') as HTMLInputElement;
const crModerated = document.getElementById('cr-moderated') as HTMLInputElement;
const crLobby = document.getElementById('cr-lobby') as HTMLInputElement;
const crSecret = document.getElementById('cr-secret') as HTMLInputElement;
const crGuests = document.getElementById('cr-guests') as HTMLInputElement;
const createRoomSubmit = document.getElementById('create-room-submit') as HTMLButtonElement;
const createRoomError = document.getElementById('create-room-error')!;

// Room settings modal
const roomSettingsModal = document.getElementById('room-settings-modal') as HTMLDialogElement;
const rsModerated = document.getElementById('rs-moderated') as HTMLInputElement;
const rsLobby = document.getElementById('rs-lobby') as HTMLInputElement;
const rsScreen = document.getElementById('rs-screen') as HTMLInputElement;
const rsChat = document.getElementById('rs-chat') as HTMLInputElement;
const rsGuests = document.getElementById('rs-guests') as HTMLInputElement;
const rsGuestsBroadcast = document.getElementById('rs-guests-broadcast') as HTMLInputElement;
const rsRequireReg = document.getElementById('rs-require-reg') as HTMLInputElement;
const rsInviteOnly = document.getElementById('rs-invite-only') as HTMLInputElement;
const rsSecret = document.getElementById('rs-secret') as HTMLInputElement;
const rsVideo = document.getElementById('rs-video') as HTMLInputElement;
const rsPtt = document.getElementById('rs-ptt') as HTMLInputElement;
const rsMaxBroadcasters = document.getElementById('rs-max-broadcasters') as HTMLInputElement;
const rsMaxParticipants = document.getElementById('rs-max-participants') as HTMLInputElement;
const rsTopic = document.getElementById('rs-topic') as HTMLInputElement;
const rsPassword = document.getElementById('rs-password') as HTMLInputElement;
const rsPasswordRemove = document.getElementById('rs-password-remove') as HTMLButtonElement;
const rsPasswordHint = document.getElementById('rs-password-hint')!;

// Toast container
const toastContainer = document.getElementById('toast-container')!;

// Sidebar tabs
const sidebarTabs = document.querySelectorAll<HTMLButtonElement>('#sidebar-tabs .tab');
const tabContents = document.querySelectorAll<HTMLDivElement>('#sidebar-content .tab-content');
const sidebarCollapseBtn = document.getElementById('sidebar-collapse') as HTMLButtonElement;

// Personal controls retain their values/listeners as they move into the shared dialog.
const layoutSelect = document.getElementById('layout-select') as HTMLSelectElement;
const micModeSelect = document.getElementById('mic-mode-select') as HTMLSelectElement;
const appearanceControls = document.getElementById('appearance-controls')!;
const microphoneControls = document.getElementById('microphone-controls')!;
document.getElementById('personal-settings-controls')!.remove();

// --- Auth ---
let capabilities: ServerCapabilities | null = null;
let capabilitiesLoading = false;
const auth = new AuthManager();
auth.setTelemetryHandler(telemetry.record);
const authFlow = new AuthDialogFlow(() => auth.cancelPasskeyAttempt());

function updateAuthUI(): void {
  signInBtn.hidden = capabilities?.accounts !== true;
  document.getElementById('community-actions')!.hidden = capabilities?.accounts !== true;
  roomBrowser.hidden = capabilities?.roomDirectory !== true;
  joinFormDivider.hidden = roomBrowser.hidden;
  createRoomBtn.hidden = !auth.isLoggedIn || capabilities?.roomCreation !== true;
  if (auth.isLoggedIn) {
    authBarGuest.hidden = true;
    authBarUser.hidden = false;
    authDisplayName.textContent = auth.displayName ?? '';
    // Pre-fill name input with auth display name
    if (auth.displayName && !nameInput.value.trim()) {
      nameInput.value = auth.displayName;
    }
  } else {
    authBarGuest.hidden = false;
    authBarUser.hidden = true;
  }
  if (!roomBrowser.hidden) observeUiTask(loadRoomBrowser(), 'Could not refresh the room directory');
  updateJoinBtn();
  community.refresh();
  participantHovercard.refresh();
}

// Invitation secrets stay in the fragment and are removed before room navigation.
let pendingInvite: string | null = null;
let pendingInviteKind: 'room' | 'registration' = 'room';
let inviteAccountEpoch = 0;
let inviteView: ReturnType<typeof modal> | null = null;
function readInviteLink(returnRoom = ''): void {
  const url = new URL(window.location.href);
  const fragment = new URLSearchParams(url.hash.slice(1));
  const registration = fragment.get('register-invite');
  const roomCode = fragment.get('invite');
  const code = registration ?? roomCode;
  if (code === null) return;
  url.hash = returnRoom;
  window.history.replaceState(null, '', `${url.pathname}${url.search}${url.hash}`);
  const normalized = code.trim().toLowerCase();
  if (!/^[abcdefghjkmnpqrstuvwxyz023456789]{32}$/.test(normalized)) {
    showToast('This invitation link is not valid', 4000, 'error');
    return;
  }
  pendingInvite = normalized;
  pendingInviteKind = registration !== null ? 'registration' : 'room';
  registerInvite.value = normalized;
}

/** Fragment links can arrive without reloading an already open application. */
function consumeInviteLocation(selectedRoom: string): boolean {
  const fragment = new URLSearchParams(window.location.hash.slice(1));
  if (!fragment.has('invite') && !fragment.has('register-invite')) return false;
  inviteView?.close();
  inviteView = null;
  pendingInvite = null;
  navigation.cancelPendingJoin();
  readInviteLink(selectedRoom);
  observeUiTask(previewPendingInvite(), 'Invitation not reviewed');
  return true;
}

/** A link can preview an offer; only its explicitly clicked acceptance mutates membership. */
async function previewPendingInvite(): Promise<void> {
  const code = pendingInvite;
  if (!code || !capabilities || inviteView?.dialog.open) return;
  if (!capabilities.accounts) {
    pendingInvite = null;
    showToast('Invitations are unavailable on this server.', 4000, 'error');
    return;
  }
  if (pendingInviteKind === 'registration') {
    openAuthDialog(registerModal);
    return;
  }
  if (!auth.isLoggedIn) {
    const view = modal('Room invitation');
    inviteView = view;
    view.body.append(
      el(
        'p',
        'Sign in to review the room and offered role. Opening this link does not accept it or join a room.',
      ),
      button('Sign in to review', () => {
        view.close();
        openAuthDialog(loginModal);
      }),
    );
    return;
  }
  pendingInvite = null;
  const epoch = inviteAccountEpoch;
  const userId = auth.userId;
  const revision = navigation.revision;
  const view = modal('Review room invitation');
  inviteView = view;
  const ownsIntent = (): boolean =>
    view.dialog.open &&
    inviteView === view &&
    epoch === inviteAccountEpoch &&
    userId === auth.userId &&
    auth.isLoggedIn &&
    revision === navigation.revision;
  const description = el('p', 'Loading invitation…');
  view.body.append(description);
  try {
    const offer = await api.previewInvite(auth.jwt, code);
    if (!ownsIntent()) return;
    description.textContent = `Room: ${offer.display_name} (${offer.room_id}). Offered role: ${offer.role}.`;
    view.body.append(
      el(
        'p',
        'Accept only if you intended to receive this role. Acceptance never joins the room or starts your microphone or camera. Existing permissions may differ when reusing an invitation.',
      ),
    );
    const accept = button(
      'Accept invitation',
      () => {
        if (!ownsIntent() || accept.disabled) return;
        accept.disabled = true;
        observeUiTask(
          (async () => {
            try {
              const accepted = await api.redeemInvite(auth.jwt, code);
              if (!ownsIntent()) return;
              view.close();
              const alreadyJoined = room?.currentRoomId === accepted.room_id;
              if (!alreadyJoined) navigation.selectRoom(accepted.room_id);
              showToast(
                `Invitation confirmed for ${accepted.display_name}. Current permissions apply.${alreadyJoined ? '' : ' Choose Join when ready.'}`,
              );
            } catch (error) {
              if (!ownsIntent()) return;
              view.error.textContent =
                error instanceof Error ? error.message : 'The invitation could not be accepted';
              view.error.hidden = false;
              // Retrying remains a separate user action, never an automatic mutation.
              accept.disabled = false;
            }
          })(),
          'The invitation could not be accepted',
        );
      },
      'btn-primary',
    );
    view.body.append(accept);
  } catch (error) {
    if (!ownsIntent()) return;
    description.textContent = 'The invitation could not be reviewed.';
    view.error.textContent = error instanceof Error ? error.message : 'Please try the link again.';
    view.error.hidden = false;
  }
}
/** An account-room action keeps its explicit join intent until signaling is ready. */
function openRoomFromDialog(id: string): void {
  if (!auth.isLoggedIn || room?.currentRoomId === id) return;
  if (auth.displayName && !nameInput.value.trim()) nameInput.value = auth.displayName;
  navigation.requestJoin(id);
}
readInviteLink();

auth.setOnChange((loggedIn, tokenRefresh) => {
  updateAuthUI();
  if (loggedIn && tokenRefresh) {
    // Renew the existing socket as well as the next handshake, preserving room
    // membership and media across the original token's expiry.
    signaling.setToken(auth.jwt ?? undefined);
    return;
  }
  // Identity changes leave the old membership before reconnecting.
  inviteAccountEpoch++;
  inviteView?.close();
  inviteView = null;
  navigation.cancelPendingJoin();
  dismissCreateRoom();
  createRoomMutation++;
  createRoomSubmit.disabled = false;
  createRoomSubmit.textContent = 'Create Room';
  if (room) observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
  signaling.disconnect();
  signaling.connect(loggedIn ? (auth.jwt ?? undefined) : undefined);
  if (loggedIn) observeUiTask(previewPendingInvite(), 'Invitation not reviewed');
});

// --- State ---
let room: RoomClient | null = null;
let roomPasswordView: { cancel: () => void } | null = null;
const mediaControls = new MediaControls({
  getRoom: () => room,
  notify: (message) => showToast(message),
  onPlaybackResult: (element, blocked) => callTelemetry.playbackResult(element, blocked),
  appearanceControls,
  microphoneControls,
});
mediaControls.mountToolbar(document.getElementById('room-volume-slot')!);
let localTextMuted = false;
let roomRecovering = false;
let cameraTogglePending = false;
let microphoneTogglePending = false;
const remoteTiles = new Map<string, HTMLDivElement>();
let pinnedTileKey: string | null = null;
/** Size observers per camera tile: the layer a tile can show caps what it requests. */
const tileLayerCaps = new Map<string, { observer: ResizeObserver; layer: number | null }>();

function observeTileSize(tileKey: string, participantId: string, video: HTMLVideoElement): void {
  if (tileLayerCaps.has(tileKey) || typeof ResizeObserver === 'undefined') return;
  const state: { observer: ResizeObserver; layer: number | null } = {
    observer: new ResizeObserver((entries) => {
      const width = entries[0]?.contentRect.width ?? video.clientWidth;
      const layer = spatialLayerForRenderedWidth(width, window.devicePixelRatio, state.layer);
      if (layer === state.layer) return;
      state.layer = layer;
      room?.setRemoteVideoSizeCap(participantId, layer);
    }),
    layer: null,
  };
  state.observer.observe(video);
  tileLayerCaps.set(tileKey, state);
}

function stopObservingTileSize(tileKey: string): void {
  const state = tileLayerCaps.get(tileKey);
  if (!state) return;
  state.observer.disconnect();
  tileLayerCaps.delete(tileKey);
}
const lobbyWaiters = new Map<string, string>(); // participantId → displayName
const lobbyActions = new Map<string, { pending: boolean; error?: string }>();
let roomSettingsPending = false;
let roomSettingsAttempt = 0;
let navigationPending = false;
let roomSelectionVersion = 0;
let departureInProgress: Promise<void> | null = null;

// Active speaker / audio level tracking — avoids querySelectorAll on every event
const AUDIO_LEVEL_THRESHOLD = -50; // dB; only highlight above this
// The server reports levels every 800 ms while anyone speaks and sends an
// empty list on silence; the timer covers a lost silence report.
const SPEAKING_HIGHLIGHT_TIMEOUT_MS = 2000;
let currentDominantTile: HTMLDivElement | null = null;
const currentlySpeaking = new Set<HTMLElement>(); // tiles + list items with .speaking
let speakingHighlightTimer: ReturnType<typeof setTimeout> | null = null;

function clearSpeakingHighlights(): void {
  if (speakingHighlightTimer !== null) {
    clearTimeout(speakingHighlightTimer);
    speakingHighlightTimer = null;
  }
  for (const el of currentlySpeaking) {
    el.classList.remove('speaking');
  }
  currentlySpeaking.clear();
  if (currentDominantTile) {
    currentDominantTile.classList.remove('dominant-speaker');
    currentDominantTile = null;
  }
}

// Persisting preferences is optional; blocked or full storage must not stop a call.
const memoryPreferences = new Map<string, string>();
function readLocalPreference(key: string): string | null {
  if (memoryPreferences.has(key)) return memoryPreferences.get(key) ?? null;
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}
function writeLocalPreference(key: string, value: string): void {
  memoryPreferences.set(key, value);
  try {
    localStorage.setItem(key, value);
  } catch {
    /* Keep the value for this tab. */
  }
}

// Push-to-Talk state
type MicMode = 'open' | 'ptt';
let personalMicMode: MicMode = readLocalPreference('micMode') === 'ptt' ? 'ptt' : 'open';
let micMode: MicMode = personalMicMode;
let pttHeld = false;
let pttActivation = 0;

// Restore display name from localStorage
const savedName = readLocalPreference('displayName');
if (savedName) nameInput.value = savedName;

// --- Utility ---
function clearChildren(el: HTMLElement): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}

// --- Toast Notifications ---
type ToastKind = 'info' | 'error';

/** The container is a polite live region; errors are additionally marked as alerts. */
function showToast(message: string, duration = 3000, kind: ToastKind = 'info'): void {
  const toast = document.createElement('div');
  toast.className = kind === 'error' ? 'toast toast-error' : 'toast';
  if (kind === 'error') toast.setAttribute('role', 'alert');
  toast.textContent = message;
  toast.addEventListener('click', () => toast.remove());
  toastContainer.appendChild(toast);
  setTimeout(() => toast.classList.add('toast-leaving'), Math.max(0, duration - 300));
  setTimeout(() => toast.remove(), duration);
}

/** Observe detached UI work without reporting an old room/account's failure to its replacement. */
function observeUiTask(task: Promise<unknown>, failureMessage: string): void {
  const expectedRoom = room;
  const expectedUser = auth.userId;
  task.catch(() => {
    // Browser/library exceptions may contain transport or account details.
    // Keep the diagnostic useful without retaining the raw error payload.
    console.error(failureMessage);
    if (room === expectedRoom && auth.userId === expectedUser)
      showToast(failureMessage, 8000, 'error');
  });
}

/** DOM callbacks return void; their asynchronous work still needs a rejection observer. */
function asyncUiAction(action: () => Promise<unknown>, failureMessage: string): () => void {
  return () => observeUiTask((async () => action())(), failureMessage);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function showActionToast(
  message: string,
  actions: { label: string; action: () => void }[],
  duration = 10000,
): HTMLElement {
  const toast = document.createElement('div');
  toast.className = 'toast toast-action';

  const msg = document.createElement('div');
  msg.textContent = message;
  toast.appendChild(msg);

  const btnRow = document.createElement('div');
  btnRow.className = 'toast-actions';
  for (const a of actions) {
    const btn = document.createElement('button');
    btn.textContent = a.label;
    btn.addEventListener('click', () => {
      a.action();
      toast.remove();
    });
    btnRow.appendChild(btn);
  }
  toast.appendChild(btnRow);

  toastContainer.appendChild(toast);
  if (duration > 0) setTimeout(() => toast.remove(), duration);
  return toast;
}

// --- Moderation Context Menu ---
function showModerationMenu(targetId: string, targetName: string, x: number, y: number): void {
  participantHovercard.close();
  document.getElementById('mod-menu')?.remove();

  const role = room?.role ?? 'user';
  const isMod = role === 'owner' || role === 'admin' || role === 'moderator';
  const isAdmin = role === 'owner' || role === 'admin';

  const owner = room;
  if (!owner) return;
  const membership = owner.membershipVersion;
  const items: { label: string; action: () => void | Promise<void>; danger?: boolean }[] = [];

  const isSelf = targetId === room?.localParticipantId;
  const authenticated = isSelf
    ? auth.isLoggedIn
    : room?.getParticipants().get(targetId)?.authenticated === true;
  if (authenticated)
    items.push({
      label: 'View profile',
      action: () => observeUiTask(community.showProfile(targetId), 'Could not open the profile'),
    });
  if (!isSelf) {
    items.push({
      label: 'Private message',
      action: () => socialChat.openPrivate(targetId, targetName),
    });
    items.push({
      label: socialChat.isIgnored(targetId) ? 'Unignore messages' : 'Ignore messages',
      action: () => {
        socialChat
          .toggleIgnore(targetId, targetName, authenticated)
          .catch((error) =>
            showToast(error instanceof Error ? error.message : 'Ignore update failed'),
          );
      },
    });
    items.push({
      label: 'Report to moderators',
      action: () => community.report(targetId, targetName),
    });
  }

  // Mod+ actions
  if (isMod && !isSelf) {
    items.push({ label: 'Close Camera', action: () => owner.closeCam(targetId) });
    items.push({ label: 'Cam Unban', action: () => owner.camUnban(targetId) });
    items.push({ label: 'Mute Text', action: () => owner.textMute(targetId) });
    items.push({ label: 'Text Unmute', action: () => owner.textUnmute(targetId) });
    items.push({ label: 'Kick', action: () => owner.kick(targetId), danger: true });
  }

  // Admin+ actions
  if (isAdmin && !isSelf) {
    items.push({ label: 'Cam Ban', action: () => owner.camBan(targetId), danger: true });
    items.push({ label: 'Ban…', action: () => community.ban(targetId, targetName), danger: true });
  }

  const menu = document.createElement('div');
  menu.id = 'mod-menu';
  menu.className = 'mod-context-menu';
  // Names can repeat or look alike; the account/guest marker and the id's start cannot.
  const target = el('div', undefined, 'mod-menu-target');
  target.append(
    el('span', targetName, 'mod-menu-target-name'),
    el('span', `${authenticated ? 'account' : 'guest'} · ${targetId.slice(0, 8)}`),
  );
  menu.append(target);

  let pending = false;
  const error = el('p', '', 'auth-error');
  error.setAttribute('role', 'alert');
  error.hidden = true;
  const apply = async (action: () => void | Promise<void>): Promise<void> => {
    if (pending || room !== owner || owner.membershipVersion !== membership) return;
    pending = true;
    error.hidden = true;
    menu.setAttribute('aria-busy', 'true');
    for (const button of menu.querySelectorAll('button')) button.disabled = true;
    try {
      await action();
      menu.remove();
    } catch (failure) {
      if (room === owner && owner.membershipVersion === membership) {
        const message = failure instanceof Error ? failure.message : 'The action was not confirmed';
        if (menu.isConnected) {
          error.textContent = message;
          error.hidden = false;
        } else showToast(message, 7000);
      }
    } finally {
      pending = false;
      menu.setAttribute('aria-busy', 'false');
      for (const button of menu.querySelectorAll('button')) button.disabled = false;
    }
  };

  for (const item of items) {
    const btn = document.createElement('button');
    btn.textContent = item.label;
    if (item.danger) btn.className = 'danger';
    btn.addEventListener(
      'click',
      asyncUiAction(() => apply(item.action), 'Could not apply room action'),
    );
    menu.appendChild(btn);
  }

  // Set Role sub-menu
  const roleOptions: { label: string; value: number }[] = [];
  if (isMod && !isSelf) {
    roleOptions.push({ label: 'User', value: 1 });
    roleOptions.push({ label: 'Member', value: 2 });
  }
  if (isAdmin && !isSelf) {
    roleOptions.push({ label: 'Moderator', value: 3 });
  }
  if (role === 'owner' && !isSelf) {
    roleOptions.push({ label: 'Admin', value: 4 });
  }

  if (roleOptions.length > 0) {
    const group = document.createElement('div');
    group.className = 'mod-menu-group';
    const label = document.createElement('div');
    label.className = 'mod-menu-label';
    label.textContent = 'Set Role';
    group.appendChild(label);

    for (const opt of roleOptions) {
      const btn = document.createElement('button');
      btn.textContent = opt.label;
      btn.addEventListener(
        'click',
        asyncUiAction(
          () => apply(() => owner.setRole(targetId, opt.value)),
          'Could not change role',
        ),
      );
      group.appendChild(btn);
    }
    menu.appendChild(group);
  }

  menu.style.left = `${Math.min(x, window.innerWidth - 180)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - (items.length + roleOptions.length + 2) * 36 - 16)}px`;
  menu.style.top = `${Math.max(8, parseInt(menu.style.top, 10))}px`;
  menu.style.maxHeight = `${window.innerHeight - 16}px`;
  menu.style.overflowY = 'auto';
  menu.append(error);
  document.body.appendChild(menu);

  const close = (e: MouseEvent) => {
    if (!pending && !menu.contains(e.target as Node)) {
      menu.remove();
      document.removeEventListener('click', close);
    }
  };
  setTimeout(() => document.addEventListener('click', close), 0);
}

// --- Role Badge Helpers ---
const ROLE_SYMBOLS: Record<string, string> = {
  owner: '~',
  admin: '&',
  moderator: '@',
  member: '+',
};
const ROLE_NAMES: Record<string, string> = {
  owner: 'Owner',
  admin: 'Admin',
  moderator: 'Moderator',
  member: 'Member',
};

function getRoleBadgeSpan(role: string): HTMLSpanElement | null {
  const symbol = ROLE_SYMBOLS[role];
  if (!symbol) return null;
  const span = document.createElement('span');
  span.className = `role-badge role-${role}`;
  span.textContent = symbol;
  const name = ROLE_NAMES[role] ?? role;
  span.title = name;
  span.setAttribute('role', 'img');
  span.setAttribute('aria-label', name);
  return span;
}

// --- Room Settings / Moderation UI Helpers ---
function updateRoomModeUI(): void {
  const role = room?.role ?? 'user';
  const settings = room?.roomSettings;

  // Show hand-raise button for non-privileged users in moderated rooms
  const isPrivileged =
    role === 'owner' || role === 'admin' || role === 'moderator' || role === 'member';
  handBtn.hidden = !(settings?.moderated && !isPrivileged);

  // Show room settings button for admins+
  roomSettingsBtn.hidden = !(role === 'owner' || role === 'admin');

  renderLobbyPanel();
}

function applyRoomSettingsToUI(): void {
  const settings = room?.roomSettings;
  const privileged = ['member', 'moderator', 'admin', 'owner'].includes(room?.role ?? 'guest');
  const needsVoice = settings?.moderated && !privileged;
  const chatDisabled =
    localTextMuted || settings?.allowChat === false || needsVoice || roomRecovering;
  chatInput.disabled = Boolean(chatDisabled);
  chatInput.placeholder = roomRecovering
    ? 'Reconnecting…'
    : localTextMuted
      ? 'You are muted in this room'
      : settings?.allowChat === false
        ? 'Chat is disabled'
        : needsVoice
          ? 'Raise your hand to request voice'
          : 'Type a message...';
  (chatSendBtn as HTMLButtonElement).disabled = Boolean(chatDisabled);
  chatSendBtn.classList.toggle('disabled', Boolean(chatDisabled));

  setMicMode(settings?.pushToTalk ? 'ptt' : personalMicMode, false);
  micModeSelect.disabled = settings?.pushToTalk === true;
  micModeSelect.title = micModeSelect.disabled
    ? 'This room requires push to talk'
    : 'Your preferred microphone mode';

  // Screen sharing: hide button when disabled
  if (settings?.allowScreenSharing === false) {
    screenBtn.hidden = true;
  } else {
    screenBtn.hidden = !navigator.mediaDevices?.getDisplayMedia;
  }

  // Video: hide cam button when disabled
  camBtn.hidden = settings?.allowVideo === false;
  socialChat.participantsChanged();
}

function renderLobbyPanel(): void {
  const role = room?.role ?? 'user';
  const lobbyEnabled =
    room?.roomSettings?.lobbyEnabled || room?.roomSettings?.inviteOnly || lobbyWaiters.size > 0;
  const canAdmit = role === 'owner' || role === 'admin' || role === 'moderator';

  // Show/hide lobby tab
  lobbyTab.hidden = !(canAdmit && lobbyEnabled);
  if (lobbyTab.hidden && lobbyTab.classList.contains('active')) selectSidebarTab('chat');

  clearChildren(lobbyListEl);
  lobbyEmpty.hidden = lobbyWaiters.size > 0;

  for (const [id, name] of lobbyWaiters) {
    const li = document.createElement('li');
    li.className = 'lobby-entry';
    li.dataset['participantId'] = id;

    const nameSpan = document.createElement('span');
    nameSpan.className = 'lobby-entry-name';
    nameSpan.textContent = name;

    const actions = document.createElement('div');
    actions.className = 'lobby-entry-actions';

    const admitBtn = document.createElement('button');
    admitBtn.className = 'lobby-admit-btn';
    admitBtn.textContent = 'Admit';
    admitBtn.addEventListener(
      'click',
      asyncUiAction(() => applyLobbyAction(id, 'admit'), 'Could not admit participant'),
    );

    const denyBtn = document.createElement('button');
    denyBtn.className = 'lobby-deny-btn';
    denyBtn.textContent = 'Deny';
    denyBtn.addEventListener(
      'click',
      asyncUiAction(() => applyLobbyAction(id, 'deny'), 'Could not deny participant'),
    );

    const state = lobbyActions.get(id);
    admitBtn.disabled = Boolean(state?.pending) || !room?.connected;
    denyBtn.disabled = admitBtn.disabled;
    li.setAttribute('aria-busy', String(Boolean(state?.pending)));
    if (state?.pending) nameSpan.append(el('span', ' · Awaiting confirmation…'));
    if (state?.error) {
      const error = el('p', state.error, 'auth-error');
      error.setAttribute('role', 'alert');
      li.append(error);
    }
    actions.appendChild(admitBtn);
    actions.appendChild(denyBtn);
    li.appendChild(nameSpan);
    li.appendChild(actions);
    lobbyListEl.appendChild(li);
  }
}

async function applyLobbyAction(id: string, action: 'admit' | 'deny'): Promise<void> {
  const owner = room;
  if (!owner || lobbyActions.get(id)?.pending) return;
  const membership = owner.membershipVersion;
  const current = () => room === owner && owner.membershipVersion === membership;
  lobbyActions.set(id, { pending: true });
  renderLobbyPanel();
  try {
    if (action === 'admit') await owner.admitFromLobby(id);
    else await owner.denyFromLobby(id);
    if (current()) {
      lobbyWaiters.delete(id);
      lobbyActions.delete(id);
    }
  } catch (error) {
    if (current()) {
      const message = error instanceof Error ? error.message : 'The result was not confirmed';
      if (lobbyWaiters.has(id)) lobbyActions.set(id, { pending: false, error: message });
      else {
        lobbyActions.delete(id);
        showToast(message);
      }
    }
  } finally {
    if (current()) renderLobbyPanel();
  }
}

function populateRoomSettingsModal(): void {
  if (roomSettingsPending) return;
  const settings = room?.roomSettings;
  if (!settings) return;
  rsModerated.checked = settings.moderated;
  rsLobby.checked = settings.lobbyEnabled;
  rsScreen.checked = settings.allowScreenSharing;
  rsChat.checked = settings.allowChat;
  rsGuests.checked = settings.guestsAllowed;
  rsGuestsBroadcast.checked = settings.guestsCanBroadcast;
  rsRequireReg.checked = settings.requireRegistration;
  rsInviteOnly.checked = settings.inviteOnly;
  rsSecret.checked = settings.secret;
  rsVideo.checked = settings.allowVideo;
  rsPtt.checked = settings.pushToTalk;
  rsMaxBroadcasters.value = settings.maxBroadcasters?.toString() ?? '';
  rsMaxParticipants.value = settings.maxParticipants?.toString() ?? '';
  rsTopic.value = settings.topic ?? '';
  rsPassword.value = '';
  rsPasswordRemove.hidden = !settings.passwordProtected;
  rsPasswordHint.textContent = settings.passwordProtected
    ? 'A password is set. Enter a new one to replace it, or remove it below.'
    : 'No password. Enter one to require it when joining.';
}

function removeRoomPassword(): Promise<void> {
  return applyRoomSetting((owner) => owner.updateRoomSettings({ password: null }));
}

// --- Layout Management ---
/**
 * Phones stack the room and landscape phones up to 900 px wide split it,
 * without the desktop panels; the queries are style.css's own.
 */
function isDesktopLayout(): boolean {
  return !window.matchMedia(
    '(max-width: 768px), (max-width: 900px) and (max-height: 500px) and (orientation: landscape)',
  ).matches;
}

function getLayout(): 'modern' | 'classic' {
  return readLocalPreference('layout') === 'modern' ? 'modern' : 'classic';
}

/** Landscape phone CSS always shows the sidebar, even after a portrait collapse. */
function isLandscapePhoneLayout(): boolean {
  return window.matchMedia(
    '(max-width: 900px) and (max-height: 500px) and (orientation: landscape)',
  ).matches;
}

function isMobilePanelCollapsed(): boolean {
  return !isDesktopLayout() && panelPreferences.mobilePanelCollapsed && !isLandscapePhoneLayout();
}

function setLayout(layout: 'modern' | 'classic'): void {
  writeLocalPreference('layout', layout);
  roomScreen.classList.remove('layout-modern', 'layout-classic');
  roomScreen.classList.add(`layout-${layout}`);
  layoutSelect.value = layout;

  // In classic mode, hide the Users tab from the sidebar (users are in the left panel)
  // and force the Chat tab active
  if (usersTab) usersTab.hidden = layout === 'classic' && isDesktopLayout();
  if (usersTab?.hidden && usersTab.classList.contains('active')) selectSidebarTab('chat');
  applyPanelPreferences();

  // Re-render participants for classic mode
  if (room) renderParticipants(room.getParticipants());
}

// Initialize layout
layoutSelect.value = getLayout();

// --- Sidebar Tabs ---
/** On phones a tab press also reopens a panel that was collapsed to its tab bar. */
function expandMobilePanel(): void {
  if (panelPreferences.mobilePanelCollapsed && !isDesktopLayout()) {
    panelPreferences.mobilePanelCollapsed = false;
    savePanelPreferences();
  }
}

sidebarTabs.forEach((tab) => {
  tab.addEventListener('click', () => {
    const target = tab.dataset['tab'];
    sidebarTabs.forEach((t) => t.classList.toggle('active', t === tab));
    tabContents.forEach((c) => c.classList.toggle('active', c.id === `${target}-panel`));
    expandMobilePanel();
    if (room) renderParticipants(room.getParticipants());
  });
});
sidebarCollapseBtn.addEventListener('click', () => {
  panelPreferences.mobilePanelCollapsed = !panelPreferences.mobilePanelCollapsed;
  savePanelPreferences();
  if (room) renderParticipants(room.getParticipants());
});

// Wire lobby tab — queried at load time so hidden tabs may not be in sidebarTabs NodeList
lobbyTab.addEventListener('click', () => {
  document
    .querySelectorAll<HTMLButtonElement>('#sidebar-tabs .tab')
    .forEach((t) => t.classList.toggle('active', t === lobbyTab));
  document
    .querySelectorAll<HTMLDivElement>('#sidebar-content .tab-content')
    .forEach((c) => c.classList.toggle('active', c.id === 'lobby-panel'));
  expandMobilePanel();
  if (room) renderParticipants(room.getParticipants());
});

// Set tab content with icons (these are static SVG literals from our icons module)
const chatTab = document.querySelector<HTMLButtonElement>('.tab[data-tab="chat"]');
const usersTab = document.querySelector<HTMLButtonElement>('.tab[data-tab="users"]');
if (chatTab) {
  chatTab.textContent = '';
  chatTab.insertAdjacentHTML('afterbegin', icons.chatIcon());
  chatTab.append(' Chat');
}
if (usersTab) {
  usersTab.textContent = '';
  usersTab.insertAdjacentHTML('afterbegin', icons.userIcon());
  usersTab.append(' People');
}

interface PanelPreferences {
  rosterWidth: number;
  chatWidth: number;
  rosterCollapsed: boolean;
  chatCollapsed: boolean;
  /** Phones stack the panel under the video; collapsed keeps only its tab bar. */
  mobilePanelCollapsed: boolean;
}

const panelPreferences: PanelPreferences = (() => {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem('panelPreferences') ?? '{}');
    if (!isRecord(saved)) throw new Error('Invalid panel preferences');
    return {
      rosterWidth: Math.max(160, Math.min(360, Number(saved['rosterWidth']) || 220)),
      chatWidth: Math.max(240, Math.min(520, Number(saved['chatWidth']) || 320)),
      rosterCollapsed: saved['rosterCollapsed'] === true,
      chatCollapsed: saved['chatCollapsed'] === true,
      mobilePanelCollapsed: saved['mobilePanelCollapsed'] === true,
    };
  } catch {
    return {
      rosterWidth: 220,
      chatWidth: 320,
      rosterCollapsed: false,
      chatCollapsed: false,
      mobilePanelCollapsed: false,
    };
  }
})();

function selectSidebarTab(tab: 'chat' | 'users' | 'lobby'): void {
  sidebarTabs.forEach((button) => button.classList.toggle('active', button.dataset['tab'] === tab));
  tabContents.forEach((content) =>
    content.classList.toggle('active', content.id === `${tab}-panel`),
  );
}

function applyPanelPreferences(): void {
  const desktop = isDesktopLayout();
  const rosterCollapsed = desktop && panelPreferences.rosterCollapsed && getLayout() === 'classic';
  const chatCollapsed = desktop && panelPreferences.chatCollapsed;
  roomScreen.style.setProperty(
    '--roster-width',
    `${rosterCollapsed ? 0 : Math.min(panelPreferences.rosterWidth, window.innerWidth * 0.3)}px`,
  );
  roomScreen.style.setProperty(
    '--chat-width',
    `${chatCollapsed ? 0 : Math.min(panelPreferences.chatWidth, window.innerWidth * 0.4)}px`,
  );
  roomScreen.classList.toggle('roster-collapsed', rosterCollapsed);
  roomScreen.classList.toggle('chat-collapsed', chatCollapsed);
  rosterToggleBtn.querySelector('.tool-label')!.textContent = rosterCollapsed
    ? 'Show people'
    : 'People';
  chatToggleBtn.querySelector('.tool-label')!.textContent = chatCollapsed ? 'Show chat' : 'Chat';
  rosterToggleBtn.setAttribute('aria-expanded', String(!rosterCollapsed));
  chatToggleBtn.setAttribute('aria-expanded', String(!chatCollapsed));
  const roster = document.getElementById('classic-users-panel');
  if (roster) roster.inert = rosterCollapsed;
  document.getElementById('sidebar')!.inert = chatCollapsed;
  if (usersTab) usersTab.hidden = desktop && getLayout() === 'classic';
  if (usersTab?.hidden && usersTab.classList.contains('active')) selectSidebarTab('chat');
  const mobileCollapsed = isMobilePanelCollapsed();
  roomScreen.classList.toggle('mobile-panel-collapsed', mobileCollapsed);
  sidebarCollapseBtn.setAttribute('aria-expanded', String(!mobileCollapsed));
  sidebarCollapseBtn.setAttribute(
    'aria-label',
    mobileCollapsed ? 'Show the chat panel' : 'Collapse the chat panel',
  );
}

function savePanelPreferences(): void {
  try {
    localStorage.setItem('panelPreferences', JSON.stringify(panelPreferences));
  } catch {
    /* Retain session preferences. */
  }
  applyPanelPreferences();
}

function attachPanelResize(panel: HTMLElement, side: 'roster' | 'chat'): void {
  if (panel.querySelector('.panel-resize-handle')) return;
  const handle = document.createElement('div');
  handle.className = `panel-resize-handle resize-${side}`;
  handle.tabIndex = 0;
  handle.setAttribute('role', 'separator');
  handle.setAttribute('aria-orientation', 'vertical');
  handle.setAttribute('aria-label', `Resize ${side === 'roster' ? 'people' : 'chat'} panel`);
  const key = side === 'roster' ? 'rosterWidth' : 'chatWidth';
  const setWidth = (width: number) => {
    panelPreferences[key] = Math.max(
      side === 'roster' ? 160 : 240,
      Math.min(side === 'roster' ? 360 : 520, width),
    );
    handle.setAttribute('aria-valuenow', String(Math.round(panelPreferences[key])));
    applyPanelPreferences();
  };
  handle.setAttribute('aria-valuemin', side === 'roster' ? '160' : '240');
  handle.setAttribute('aria-valuemax', side === 'roster' ? '360' : '520');
  handle.setAttribute('aria-valuenow', String(panelPreferences[key]));
  handle.addEventListener('pointerdown', (event) => {
    if (!isDesktopLayout() || event.button !== 0) return;
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = panelPreferences[key];
    const move = (next: PointerEvent) => {
      if (next.pointerId !== event.pointerId) return;
      setWidth(startWidth + (next.clientX - startX) * (side === 'roster' ? 1 : -1));
    };
    const stop = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', stop);
      window.removeEventListener('pointercancel', stop);
      window.removeEventListener('blur', stop);
      savePanelPreferences();
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', stop);
    window.addEventListener('pointercancel', stop);
    window.addEventListener('blur', stop);
  });
  handle.addEventListener('keydown', (event) => {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    setWidth(
      panelPreferences[key] +
        (event.key === 'ArrowRight' ? 20 : -20) * (side === 'roster' ? 1 : -1),
    );
    savePanelPreferences();
  });
  panel.append(handle);
}

rosterToggleBtn.addEventListener('click', () => {
  if (getLayout() === 'classic' && isDesktopLayout()) {
    panelPreferences.rosterCollapsed = !panelPreferences.rosterCollapsed;
  } else {
    panelPreferences.chatCollapsed = false;
    selectSidebarTab('users');
    expandMobilePanel();
  }
  savePanelPreferences();
  if (room) renderParticipants(room.getParticipants());
});
chatToggleBtn.addEventListener('click', () => {
  if (isDesktopLayout()) panelPreferences.chatCollapsed = !panelPreferences.chatCollapsed;
  selectSidebarTab('chat');
  savePanelPreferences();
  if (room) renderParticipants(room.getParticipants());
});
attachPanelResize(document.getElementById('sidebar')!, 'chat');
let rosterOnDesktop = isDesktopLayout();
let rosterInLandscapePhone = isLandscapePhoneLayout();
window.addEventListener('resize', () => {
  applyPanelPreferences();
  const desktop = isDesktopLayout();
  const landscape = isLandscapePhoneLayout();
  if ((desktop !== rosterOnDesktop || landscape !== rosterInLandscapePhone) && room)
    renderParticipants(room.getParticipants());
  rosterOnDesktop = desktop;
  rosterInLandscapePhone = landscape;
});
document.getElementById('mic-setup-btn')!.addEventListener(
  'click',
  asyncUiAction(() => mediaControls.openSetup('microphone'), 'Could not open microphone settings'),
);
document.getElementById('copy-room-link')!.addEventListener(
  'click',
  asyncUiAction(async () => {
    const activeRoom = room;
    if (!activeRoom?.currentRoomId) return;
    const url = new URL(window.location.href);
    url.hash = activeRoom.currentRoomId;
    url.search = '';
    // Phones offer their share sheet; desktops copy, as people expect there.
    if (!isDesktopLayout() && typeof navigator.share === 'function') {
      try {
        await navigator.share({
          title: activeRoom.roomSettings?.displayName ?? activeRoom.currentRoomId,
          url: url.toString(),
        });
        return;
      } catch (error) {
        if (error instanceof DOMException && error.name === 'AbortError') return;
      }
    }
    try {
      await navigator.clipboard.writeText(url.toString());
      if (room === activeRoom) showToast('Room link copied');
    } catch {
      if (room === activeRoom) showCopyFallback(url.toString());
    }
  }, 'Could not copy the room link'),
);

for (const [id, icon] of [
  ['copy-room-link', icons.link()],
  ['toggle-roster', icons.userIcon()],
  ['toggle-chat', icons.chatIcon()],
] as const)
  document.getElementById(id)!.insertAdjacentHTML('afterbegin', icon);
document.getElementById('room-more-btn')!.insertAdjacentHTML('beforeend', icons.chevronDown());
document.getElementById('shortcuts-btn')!.addEventListener('click', showKeyboardShortcuts);
setupRoomMenu();
if (typeof ResizeObserver !== 'undefined') {
  const bars = new ResizeObserver(placeToasts);
  bars.observe(document.querySelector('header')!);
  bars.observe(roomTools);
}

/** Toasts sit below the shared header, clear of its controls. */
function placeToasts(): void {
  const top = Math.round(document.querySelector('header')!.getBoundingClientRect().bottom) + 12;
  toastContainer.style.setProperty('--toast-top', `${top}px`);
}

/** "More" holds the room tools needed now and then; any choice, Escape or a click away closes it. */
function setupRoomMenu(): void {
  const toggle = document.getElementById('room-more-btn') as HTMLButtonElement;
  const menu = document.getElementById('room-more-menu')!;
  const wrapper = toggle.parentElement!;
  const setOpen = (open: boolean): void => {
    menu.hidden = !open;
    toggle.setAttribute('aria-expanded', String(open));
    if (open) menu.querySelector<HTMLElement>('button:not([hidden]):not(:disabled)')?.focus();
  };
  toggle.addEventListener('click', () => setOpen(toggle.getAttribute('aria-expanded') !== 'true'));
  // Focus returns to the toggle before a choice runs, so a dialog it opens restores
  // focus there instead of to a menu item that is about to be hidden.
  menu.addEventListener(
    'click',
    (event) => {
      if ((event.target as Element).closest('button, a')) toggle.focus();
    },
    true,
  );
  menu.addEventListener('click', (event) => {
    if ((event.target as Element).closest('button, a')) setOpen(false);
  });
  wrapper.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape' || menu.hidden) return;
    event.stopPropagation();
    setOpen(false);
    toggle.focus();
  });
  wrapper.addEventListener('focusout', (event) => {
    if (!menu.hidden && !wrapper.contains(event.relatedTarget as Node | null)) setOpen(false);
  });
  document.addEventListener('click', (event) => {
    if (!menu.hidden && !wrapper.contains(event.target as Node)) setOpen(false);
  });
}

/** Every global shortcut in one place; the call buttons also name their own. */
function showKeyboardShortcuts(): void {
  const view = modal('Keyboard shortcuts');
  const list = el('dl', undefined, 'shortcut-list');
  const keys = (...names: string[]): HTMLElement => {
    const term = el('dt');
    names.forEach((name, index) => {
      if (index) term.append(' or ');
      term.append(el('kbd', name));
    });
    return term;
  };
  list.append(
    keys('M'),
    el('dd', 'Turn your microphone on or off'),
    keys('V'),
    el('dd', 'Turn your camera on or off'),
    keys('S'),
    el('dd', 'Start or stop sharing your screen'),
    keys('Space', 'T'),
    el('dd', 'Hold to talk, in push-to-talk mode'),
    keys('Esc'),
    el('dd', 'Close a menu or dialog'),
  );
  view.body.append(
    el('p', 'Shortcuts work in a room whenever you are not typing in a text box.', 'setting-hint'),
    list,
    el(
      'p',
      'In chat: Enter sends on desktop; Shift+Enter adds a new line. On touch devices, Enter adds a new line and the send button sends. Ctrl+Enter or ⌘+Enter sends on either.',
      'setting-hint',
    ),
  );
}

/** Keep secondary room tools in the header and return home utilities when leaving. */
function setRoomToolsVisible(visible: boolean): void {
  roomTools.hidden = !visible;
  const header = document.querySelector('header')!;
  header.classList.toggle('in-room', visible);
  const utilities = document.querySelector('.header-right')!;
  const target = visible ? document.getElementById('room-more-menu')! : header;
  if (utilities.parentElement !== target) target.append(utilities);
  document.getElementById('room-more-menu')!.hidden = true;
  document.getElementById('room-more-btn')!.setAttribute('aria-expanded', 'false');
}

/** Clipboard access can be refused; offer the link in a selectable field instead. */
function showCopyFallback(url: string): void {
  const view = modal('Copy room link');
  view.body.append(el('p', 'Clipboard access is unavailable here. Select the link to copy it.'));
  const field = el('input');
  field.type = 'text';
  field.readOnly = true;
  field.value = url;
  field.setAttribute('aria-label', 'Room link');
  view.body.append(field);
  field.focus();
  field.select();
}

/** Owned replacement for native alerts at the moments people are removed or refused. */
function showRoomExitNotice(title: string, message: string): void {
  const view = modal(title);
  view.body.append(el('p', message, 'room-exit-message'));
  view.body.append(button('Back to home', () => view.close(), 'btn-primary'));
}

/** Diagnostics sit on the home card outside a room and in the room tools' "More" menu inside one. */
function placeDiagnosticsButton(inRoom: boolean): void {
  const target = document.getElementById(inRoom ? 'room-more-repair' : 'home-tools');
  if (target && diagnosticsButton.parentNode !== target) target.append(diagnosticsButton);
}

/** Fresh sessions never capture on entry; say so and move focus into the room. */
function announceRoomEntry(roomName: string): void {
  roomScreen.focus();
  showToast(
    `Joined ${roomName}. Your camera and microphone stay off until you turn them on.`,
    6000,
  );
}

// --- Set button icons (static SVG from our icons module, no user content) ---
function setButtonContent(btn: HTMLElement, iconHtml: string, tooltip?: string): void {
  btn.textContent = '';
  btn.insertAdjacentHTML('afterbegin', iconHtml);
  if (tooltip) {
    const span = document.createElement('span');
    span.className = 'btn-tooltip';
    span.textContent = tooltip;
    btn.appendChild(span);
  }
}

setButtonContent(chatSendBtn, icons.send());
setButtonContent(micBtn, icons.micOn(), 'Mic (M)');
setButtonContent(camBtn, icons.camOn(), 'Cam (V)');
setButtonContent(screenBtn, icons.screenShare(), 'Screen (S)');
// Hide screen share button if getDisplayMedia is not supported (mobile)
if (!navigator.mediaDevices?.getDisplayMedia) {
  screenBtn.hidden = true;
}
setButtonContent(handBtn, icons.handRaised(), 'Raise Hand');
setButtonContent(roomSettingsBtn, icons.roomSettings(), 'Room Settings');
setButtonContent(settingsBtn, icons.settings(), 'Your settings');
setButtonContent(leaveBtn, icons.leave(), 'Leave');
setButtonContent(sidebarCollapseBtn, icons.chevronDown());

// --- Scroll-to-bottom for chat ---
scrollBottomBtn.textContent = '';
scrollBottomBtn.insertAdjacentHTML('afterbegin', icons.scrollDown());
scrollBottomBtn.appendChild(unreadBadge);

// --- Signaling setup ---
const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
const wsUrl = `${wsProtocol}//${window.location.host}/ws`;
const signaling = new SignalingClient(wsUrl, async (token, signal) => {
  const reply = await api.websocketTicket(token, signal);
  return { ticket: reply.ticket, expiresIn: reply.expires_in };
});
signaling.setTelemetryHandler(telemetry.record);
const callTelemetry = new CallOutcomeTelemetry(
  () =>
    room?.telemetryCallState() ?? {
      settled: false,
      rosterKnown: false,
      expected: 0,
      selected: 0,
      unavailable: false,
    },
  () => {
    const elements = new Map<MediaStreamTrack, HTMLMediaElement>();
    for (const tile of remoteTiles.values()) {
      for (const element of tile.querySelectorAll<HTMLMediaElement>('audio, video')) {
        if (!(element.srcObject instanceof MediaStream)) continue;
        for (const track of element.srcObject.getTracks()) {
          if (!elements.has(track)) elements.set(track, element);
        }
      }
    }
    return (room?.telemetrySources() ?? []).flatMap((source) => {
      const element = source.track ? elements.get(source.track) : undefined;
      return element ? [{ source, element }] : [];
    });
  },
  telemetry.record,
  () => telemetry.nextAttemptId(),
);
const mediaTelemetry = new MediaTelemetry(
  () => (telemetry.sharingEnabled ? (room?.telemetrySources() ?? []) : []),
  telemetry.record,
  callTelemetry,
  telemetry.recordMediaSample,
);
window.addEventListener('pagehide', (event) => {
  if (event.persisted) return;
  navigation.dispose();
  accountSync.dispose();
  mediaTelemetry.dispose();
  callTelemetry.dispose();
  telemetry.dispose();
  participantHovercard.destroy();
});
const socialChat = new SocialChat({
  telemetry: telemetry.record,
  getRoom: () => room,
  getViewerKey: () => auth.userId ?? 'guest',
  getToken: () => auth.jwt,
  notify: (message) => showToast(message),
  bindParticipantName: (anchor, id, name) => participantHovercard.bind(anchor, id, name),
});
const community = new CommunityUI({
  auth,
  getRoom: () => room,
  notify: (message) => showToast(message),
  onProfileChanged: (profile) => {
    participantHovercard.reset();
    auth.updateDisplayName(profile.display_name);
    if (room) renderParticipants(room.getParticipants());
  },
  onRoomsChanged: () => observeUiTask(loadRoomBrowser(), 'Could not refresh the room directory'),
  onForgetDevice: () => observeUiTask(signOutAndForget(), 'Could not sign out'),
  onJoinRoom: openRoomFromDialog,
  onRoomDeleted: async (id) => {
    if (room?.currentRoomId === id) await leaveCurrentRoom();
  },
  onSignedOut: async () => {
    await leaveCurrentRoom();
    auth.forgetSession();
  },
});

let hovercardOwner: RoomClient | null = null;
let hovercardMembership = -1;
let hovercardViewer: string | null = null;
let hovercardSession = {};
const participantHovercard = new ParticipantHovercard({
  getSession: () => {
    if (
      hovercardOwner !== room ||
      hovercardMembership !== (room?.membershipVersion ?? -1) ||
      hovercardViewer !== auth.userId
    ) {
      hovercardOwner = room;
      hovercardMembership = room?.membershipVersion ?? -1;
      hovercardViewer = auth.userId;
      hovercardSession = {};
    }
    return hovercardSession;
  },
  getParticipant: (id, fallbackName) => {
    if (!room?.localParticipantId || !room.connected) return null;
    const self = id === room.localParticipantId;
    const person = room.getParticipants().get(id);
    const online = self || !!person;
    const role = self ? room.role : person?.role;
    const color = self ? room.chatStyle?.color : person?.chatStyle?.color;
    return {
      id,
      name: self ? room.nickname : (person?.name ?? fallbackName),
      online,
      self,
      ...(role && ROLE_NAMES[role] && { role: ROLE_NAMES[role] }),
      ...(color && { color }),
      profileAvailable: self ? auth.isLoggedIn : person?.authenticated === true,
      canMessage: online && !self && room.canChat,
      canMore: online && !self,
    };
  },
  loadProfile: async (id) => {
    const profile = await community.participantProfile(id);
    return profile
      ? {
          displayName: profile.display_name,
          bio: profile.bio,
          profileStyle: profile.profile_style,
          ...(profile.avatar_url && { avatarUrl: profile.avatar_url }),
        }
      : null;
  },
  onMessage: (id, name) => socialChat.openPrivate(id, name),
  onProfile: (id) => observeUiTask(community.showProfile(id), 'Could not open the profile'),
  onMore: (id, name, anchor) => {
    const bounds = anchor.getBoundingClientRect();
    showModerationMenu(id, name, bounds.left, bounds.bottom);
  },
});

// A hidden tab outside a room lets the server's idle close stand instead of
// reconnecting every five minutes; the page reconnects when it is shown again.
signaling.setReconnectGate(
  () => Boolean(room?.currentRoomId) || document.visibilityState !== 'hidden',
);
const mediaLifecycle = new MediaLifecycle({
  getRoom: () => room,
  setPageActive: (active) => mediaControls.setPageActive(active),
  resumePlayback: () => mediaControls.resumePlayback(),
  refreshDevices: () => mediaControls.refreshDevices(),
  resumeSignaling: () => signaling.resumeDeferredReconnect(),
});
window.addEventListener('pagehide', (event) => {
  if (!event.persisted) mediaLifecycle.dispose();
});

signaling.setOnStatusChange((status) => {
  const awaitingRoom = status === 'connected' && roomRecovering;
  connectionStatus.textContent = awaitingRoom
    ? 'Rejoining room…'
    : status.charAt(0).toUpperCase() + status.slice(1);
  connectionStatus.className = `status ${awaitingRoom ? 'connecting' : status}`;
  updateJoinBtn();
  document.getElementById('connection-retry-notice')?.remove();
  if (status === 'connected' && !room?.currentRoomId) signaling.completeRestartRecovery();
  // Let the socket finish notifying its existing recovery owner before a new
  // room registers callbacks. The navigation rechecks ownership and readiness.
  if (status === 'connected') queueMicrotask(() => navigation.resumePendingJoin());
  if (status === 'disconnected' && signaling.reconnectExhausted && !room?.currentRoomId) {
    const notice = showActionToast(
      'The server is still unavailable. Retry when ready.',
      [{ label: 'Retry connection', action: () => signaling.retryConnection() }],
      0,
    );
    notice.id = 'connection-retry-notice';
  }
});

const navigation = new RoomNavigation({
  interceptLocation: consumeInviteLocation,
  leave: leaveCurrentRoom,
  select: (id) => {
    roomSelectionVersion++;
    if (inviteView?.dialog.open) {
      inviteView.close();
      pendingInvite = null;
    }
    roomInput.value = id;
    updateJoinBtn();
    if (document.activeElement?.closest('.room-card')) {
      if (!joinBtn.disabled) joinBtn.focus();
      else if (!nameInput.value.trim()) nameInput.focus();
      else roomInput.focus();
    }
  },
  pending: (pending) => {
    navigationPending = pending;
    updateJoinBtn();
  },
  error: (error) => showToast(error instanceof Error ? error.message : 'Could not change rooms'),
  tryJoin: (id) => {
    updateJoinBtn();
    if (joinBtn.disabled || roomInput.value.trim() !== id) return false;
    joinBtn.click();
    return true;
  },
});

const accountSync = new AccountSessionSync({
  reconcile: () => auth.reconcileSharedSession(),
  invalidate: () => {
    authFlow.retire();
    dismissAuth();
    auth.invalidateSharedSession();
  },
  canCheck: () => auth.canCheckSharedSession,
});
auth.setSessionMutationHandler(() => accountSync.publish());

/** Adapt onboarding to the host's supported account and room flows. */
function applyCapabilities(value: ServerCapabilities): void {
  capabilities = value;
  signInBtn.hidden = !value.accounts;
  loginSubmit.hidden = !value.passwordLogin;
  loginEmail.closest('.setting-group')!.toggleAttribute('hidden', !value.passwordLogin);
  loginPassword.closest('.setting-group')!.toggleAttribute('hidden', !value.passwordLogin);
  loginPasskeyBtn.hidden = !value.passkeyLogin;
  loginToRegister.hidden =
    value.passwordRegistration === 'disabled' && value.passkeyRegistration === 'disabled';
  registerSubmit.hidden = value.passwordRegistration === 'disabled';
  registerPasskeyBtn.hidden = value.passkeyRegistration !== 'open';
  registerInvite
    .closest('.setting-group')!
    .toggleAttribute('hidden', value.passwordRegistration !== 'invite' && !pendingInvite);
  document.getElementById('registration-help')!.textContent =
    value.passwordRegistration === 'invite'
      ? 'An invite code lets you create a password account. You can add a passkey later when this server supports it.'
      : 'Choose a sign-in method supported by this server.';
  serverMode.hidden = value.accounts || value.roomDirectory;
  serverMode.textContent = value.adHocRooms
    ? 'Guest rooms are available. Enter your name and a room ID to join.'
    : 'Rooms are not available on this server yet. Contact its host for help.';
  updateAuthUI();
}
/** Discovery owns a single bounded read; failure never guesses available actions. */
async function loadCapabilities(): Promise<void> {
  if (capabilities || capabilitiesLoading) return;
  capabilitiesLoading = true;
  capabilitiesRetry.disabled = true;
  serverMode.hidden = false;
  serverMode.textContent = 'Checking server features…';
  try {
    applyCapabilities(await api.capabilities());
    navigation.resumePendingJoin();
    observeUiTask(previewPendingInvite(), 'Invitation not reviewed');
  } catch {
    serverMode.textContent =
      'Server features could not be loaded. Check your connection and try again.';
  } finally {
    capabilitiesLoading = false;
    capabilitiesRetry.disabled = false;
    if (capabilities && document.activeElement === capabilitiesRetry) nameInput.focus();
    capabilitiesRetry.hidden = capabilities !== null;
  }
}
capabilitiesRetry.addEventListener(
  'click',
  asyncUiAction(loadCapabilities, 'Could not load server features'),
);
observeUiTask(loadCapabilities(), 'Could not load server features');

// Try to restore auth session from cookie, then connect WS
observeUiTask(
  auth.tryRestore().then(() => {
    updateAuthUI();
    signaling.connect(auth.jwt ?? undefined);
    if (!pendingInvite) return;
    observeUiTask(previewPendingInvite(), 'Invitation not reviewed');
  }),
  'Could not restore sign-in. Reload the page to retry.',
);

// --- Join form ---
function updateJoinBtn(): void {
  joinBtn.disabled =
    capabilities === null ||
    (!capabilities.roomDirectory && !capabilities.adHocRooms) ||
    navigationPending ||
    departureInProgress !== null ||
    room !== null ||
    !signaling.connected ||
    !nameInput.value.trim() ||
    !/^[A-Za-z0-9_-]{1,128}$/.test(roomInput.value.trim());
}

nameInput.addEventListener('input', () => {
  updateJoinBtn();
  navigation.resumePendingJoin();
});
roomInput.addEventListener('input', () => {
  navigation.cancelPendingJoin();
  roomSelectionVersion++;
  updateJoinBtn();
});

nameInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') joinBtn.click();
});
roomInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') joinBtn.click();
});

// --- Room Browser ---
let roomBrowserPage = 1;
let roomBrowserQuery = '';
let roomBrowserHasMore = false;
let roomBrowserRequest = 0;
let roomBrowserController: AbortController | null = null;
const MIN_ROOM_SEARCH_TRIGRAM_LEN = 3;

function hasIndexableRoomSearchTrigram(query: string): boolean {
  let runLength = 0;
  for (const character of query) {
    if (/^[A-Za-z0-9]$/.test(character)) {
      runLength++;
      if (runLength >= MIN_ROOM_SEARCH_TRIGRAM_LEN) return true;
    } else {
      runLength = 0;
    }
  }
  return false;
}

function showRoomSearchMinimum(): void {
  roomBrowserRequest++;
  roomBrowserController?.abort();
  roomBrowserPage = 1;
  roomBrowserHasMore = false;
  roomLoadMore.hidden = true;
  clearChildren(roomList);
  const message = document.createElement('div');
  message.className = 'room-list-empty';
  message.textContent = 'Enter at least 3 consecutive letters or numbers';
  roomList.appendChild(message);
}

async function loadRoomBrowser(append = false): Promise<void> {
  if (!capabilities?.roomDirectory) return;
  const request = ++roomBrowserRequest;
  roomBrowserController?.abort();
  const controller = new AbortController();
  roomBrowserController = controller;
  if (!append) {
    roomBrowserPage = 1;
    clearChildren(roomList);
    roomLoadMore.hidden = true;
  }
  roomLoadMore.disabled = true;
  try {
    const params = new URLSearchParams({ page: String(roomBrowserPage), limit: '20' });
    if (roomBrowserQuery) params.set('q', roomBrowserQuery);
    const rooms = await api.rooms(params, controller.signal).catch((error: unknown) => {
      if (!(error instanceof ApiError)) throw error;
      const explanation =
        error.status === 404 || error.status === 503
          ? 'The room directory is temporarily unavailable. You can still try joining a room by its ID below.'
          : error.status === 429
            ? 'Too many room searches. Wait a moment and try again.'
            : 'Could not load the room directory. You can still join directly by room name below.';
      throw new Error(explanation);
    });
    if (request !== roomBrowserRequest) return;
    roomBrowserHasMore = rooms.length === 20;
    roomLoadMore.hidden = !roomBrowserHasMore;

    if (rooms.length === 0 && !append) {
      const empty = document.createElement('div');
      empty.className = 'room-list-empty';
      empty.textContent = roomBrowserQuery
        ? 'No rooms match your search'
        : auth.isLoggedIn
          ? 'No public rooms yet — create one or join by name below.'
          : 'No public rooms are listed. Join by room name below, or sign in to create a saved room.';
      roomList.appendChild(empty);
      return;
    }

    for (const r of rooms) {
      const card = document.createElement('div');
      card.className = 'room-card';
      card.tabIndex = 0;
      card.setAttribute('role', 'button');
      card.setAttribute('aria-label', `Select room ${r.display_name}`);
      card.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          card.click();
        }
      });
      card.addEventListener('click', () => {
        navigation.selectRoom(r.id);
        if (auth.displayName && !nameInput.value.trim()) {
          nameInput.value = auth.displayName;
        }
        updateJoinBtn();
      });

      const info = document.createElement('div');
      info.className = 'room-card-info';
      if (safeRasterUrl(r.image_url)) {
        const image = document.createElement('img');
        image.src = r.image_url;
        image.alt = '';
        image.className = 'room-card-image';
        card.append(image);
      }

      const nameRow = document.createElement('div');
      nameRow.className = 'room-card-name';
      const nameText = document.createElement('span');
      nameText.textContent = r.display_name;
      nameRow.appendChild(nameText);
      if (r.password_protected) {
        const lockSpan = document.createElement('span');
        lockSpan.insertAdjacentHTML(
          'afterbegin',
          '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>',
        );
        nameRow.appendChild(lockSpan);
      }
      info.appendChild(nameRow);
      const roomId = document.createElement('div');
      roomId.className = 'room-card-id';
      roomId.textContent = r.id;
      info.appendChild(roomId);

      if (r.topic || r.description) {
        const topic = document.createElement('div');
        topic.className = 'room-card-topic';
        topic.textContent = r.topic || r.description || '';
        info.appendChild(topic);
      }
      if (r.topic && r.description) {
        const description = document.createElement('p');
        description.className = 'room-card-topic';
        description.textContent = r.description;
        description.title = r.description;
        info.appendChild(description);
      }

      const meta = document.createElement('div');
      meta.className = 'room-card-meta';
      const count = document.createElement('span');
      count.textContent = `${r.participant_count ?? '?'} online · ${r.broadcaster_count ?? '?'} broadcasting`;
      meta.appendChild(count);
      if (r.moderated) {
        const badge = document.createElement('span');
        badge.className = 'room-card-badge';
        badge.textContent = 'Moderated';
        meta.appendChild(badge);
      }

      card.appendChild(info);
      card.appendChild(meta);
      roomList.appendChild(card);
    }
  } catch (e) {
    if (request !== roomBrowserRequest) return;
    roomBrowserHasMore = false;
    roomLoadMore.hidden = true;
    roomList.querySelector('.directory-status')?.remove();
    const message = document.createElement('div');
    message.className = 'room-list-empty directory-status';
    message.setAttribute('role', 'status');
    message.textContent =
      e instanceof Error && !(e instanceof TypeError)
        ? e.message
        : 'The room directory could not be reached. You can still try joining directly below.';
    const retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'auth-link-btn';
    retry.textContent = 'Retry directory';
    retry.addEventListener(
      'click',
      asyncUiAction(() => loadRoomBrowser(), 'Could not refresh the room directory'),
    );
    message.append(document.createElement('br'), retry);
    roomList.append(message);
  } finally {
    if (request === roomBrowserRequest) roomLoadMore.disabled = false;
  }
}

let searchDebounceTimer: ReturnType<typeof setTimeout> | null = null;
roomSearchInput.addEventListener('input', () => {
  if (searchDebounceTimer) clearTimeout(searchDebounceTimer);
  searchDebounceTimer = setTimeout(() => {
    const query = roomSearchInput.value.trim();
    if (query && !hasIndexableRoomSearchTrigram(query)) {
      roomBrowserQuery = '';
      showRoomSearchMinimum();
      return;
    }
    roomBrowserQuery = query;
    observeUiTask(loadRoomBrowser(), 'Could not refresh the room directory');
  }, 300);
});

roomLoadMore.addEventListener('click', () => {
  roomBrowserPage++;
  observeUiTask(loadRoomBrowser(true), 'Could not load more rooms');
});

// --- Auth Events ---
function openAuthDialog(dialog: HTMLDialogElement): void {
  if (!capabilities?.accounts) return;
  if (
    dialog === registerModal &&
    capabilities.passwordRegistration === 'disabled' &&
    capabilities.passkeyRegistration === 'disabled'
  ) {
    showToast('Account registration is unavailable on this server.', 4000, 'error');
    return;
  }
  if (!dismissAuth()) return;
  dialog.hidden = false;
  dialog.showModal();
  (dialog === loginModal ? loginEmail : registerEmail).focus();
}
for (const dialog of [loginModal, registerModal]) {
  dialog.addEventListener('cancel', (event) => {
    event.preventDefault();
    dismissAuth();
  });
}
createRoomModal.addEventListener('cancel', (event) => {
  event.preventDefault();
  dismissCreateRoom();
});
function clearAuthSecrets(): void {
  loginPassword.value = '';
  registerPassword.value = '';
  registerConfirm.value = '';
}

function authSessionPending(pending: boolean): void {
  for (const element of [loginClose, registerClose, loginToRegister, registerToLogin]) {
    (element as HTMLButtonElement).disabled = pending;
    element.setAttribute('aria-disabled', String(pending));
  }
  loginModal.setAttribute('aria-busy', String(pending));
  registerModal.setAttribute('aria-busy', String(pending));
}

function showAuthFailure(
  attempt: AuthDialogAttempt,
  node: HTMLElement,
  error: unknown,
  message: string,
): void {
  if (!authFlow.current(attempt)) return;
  if (error instanceof SessionOutcomeUnknownError) {
    authFlow.markUncertain(attempt);
    clearAuthSecrets();
    authSessionPending(true);
    loginModal.setAttribute('aria-busy', 'false');
    registerModal.setAttribute('aria-busy', 'false');
    loginSubmit.disabled = true;
    loginPasskeyBtn.disabled = true;
    registerSubmit.disabled = true;
    registerPasskeyBtn.disabled = true;
    loginSubmit.textContent = 'Response not confirmed';
    registerSubmit.textContent = 'Response not confirmed';
    node.replaceChildren(
      el('p', error.message),
      button('Reload and check session', () => window.location.reload(), 'btn-primary'),
    );
  } else node.textContent = message;
  node.hidden = false;
}

function dismissAuth(): boolean {
  if (loginModal.hidden && registerModal.hidden) return true;
  if (!authFlow.dismiss()) return false;
  loginModal.hidden = true;
  registerModal.hidden = true;
  loginModal.close();
  registerModal.close();
  clearAuthSecrets();
  loginError.hidden = true;
  loginError.textContent = '';
  registerError.hidden = true;
  registerError.textContent = '';
  authSessionPending(false);
  loginSubmit.disabled = false;
  loginPasskeyBtn.disabled = false;
  registerSubmit.disabled = false;
  registerPasskeyBtn.disabled = false;
  loginSubmit.textContent = 'Sign In';
  registerSubmit.textContent = 'Create Account';
  loginPasskeyBtn.textContent = 'Use a passkey';
  registerPasskeyBtn.textContent = 'Register with passkey';
  return true;
}

signInBtn.addEventListener('click', () => {
  openAuthDialog(loginModal);
});
/** Clears names, devices, layout and chat preferences; preserves the credential-free cross-tab change marker. */
function forgetThisDevice(): void {
  const keys: string[] = [];
  for (let index = 0; index < localStorage.length; index++) {
    const key = localStorage.key(index);
    if (key !== null) keys.push(key);
  }
  for (const key of keys)
    if (
      key.startsWith('simplestchat.') ||
      ['displayName', 'micMode', 'layout', 'panelPreferences', 'reliabilityTelemetry'].includes(key)
    )
      localStorage.removeItem(key);
}

/** For a shared computer: sign out, clear this browser's memory of the app, start fresh. */
async function signOutAndForget(): Promise<void> {
  const epoch = inviteAccountEpoch;
  await auth.logout();
  // A replacement identity owns its own data, even if this continuation runs late.
  if (auth.isLoggedIn || inviteAccountEpoch !== epoch + 1) return;
  memoryPreferences.clear();
  nameInput.value = '';
  try {
    forgetThisDevice();
  } catch {
    throw new Error(
      'Signed out, but this browser prevented clearing saved app data. Clear site data in your browser settings.',
    );
  }
  location.reload();
}

logoutBtn.addEventListener('click', () => {
  signOutAndForget().catch((error) => {
    showToast(error instanceof Error ? error.message : 'Sign out failed');
  });
});
loginClose.addEventListener('click', dismissAuth);
registerClose.addEventListener('click', dismissAuth);
loginModal.addEventListener('click', (event) => {
  if (event.target === loginModal) dismissAuth();
});
registerModal.addEventListener('click', (event) => {
  if (event.target === registerModal) dismissAuth();
});
loginToRegister.addEventListener('click', () => {
  openAuthDialog(registerModal);
});
registerToLogin.addEventListener('click', () => {
  openAuthDialog(loginModal);
});

loginSubmit.addEventListener(
  'click',
  asyncUiAction(async () => {
    const attempt = authFlow.begin(true);
    if (!attempt) return;
    authSessionPending(true);
    loginError.hidden = true;
    loginSubmit.disabled = true;
    loginPasskeyBtn.disabled = true;
    loginSubmit.textContent = 'Signing in...';
    try {
      await telemetry.measure('password_login', () =>
        auth.login(loginEmail.value.trim(), loginPassword.value),
      );
      if (authFlow.finish(attempt)) {
        dismissAuth();
        loginEmail.value = '';
      }
    } catch (error) {
      showAuthFailure(
        attempt,
        loginError,
        error,
        error instanceof Error ? error.message : 'Login failed',
      );
    } finally {
      if (authFlow.current(attempt) && !authFlow.isUncertain(attempt)) {
        authFlow.finish(attempt);
        authSessionPending(false);
        clearAuthSecrets();
        loginSubmit.disabled = false;
        loginPasskeyBtn.disabled = false;
        loginSubmit.textContent = 'Sign In';
      }
    }
  }, 'Could not complete sign-in'),
);
loginEmail.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') loginPassword.focus();
});
loginPassword.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') loginSubmit.click();
});

registerSubmit.addEventListener(
  'click',
  asyncUiAction(async () => {
    registerError.hidden = true;
    try {
      validatePassword(registerPassword.value, registerConfirm.value);
    } catch (error) {
      registerError.textContent =
        error instanceof Error ? error.message : 'Choose a valid password';
      registerError.hidden = false;
      return;
    }
    const attempt = authFlow.begin(true);
    if (!attempt) return;
    authSessionPending(true);
    registerSubmit.disabled = true;
    registerPasskeyBtn.disabled = true;
    registerSubmit.textContent = 'Creating account...';
    // The registration spends this code; it is not a room invitation to redeem.
    if (pendingInvite && registerInvite.value.trim().toLowerCase() === pendingInvite)
      pendingInvite = null;
    try {
      await telemetry.measure('password_register', () =>
        auth.register(
          registerEmail.value.trim(),
          registerName.value.trim(),
          registerPassword.value,
          registerInvite.value.trim() || undefined,
        ),
      );
      if (authFlow.finish(attempt)) {
        dismissAuth();
        registerEmail.value = '';
        registerName.value = '';
        registerInvite.value = '';
      }
    } catch (error) {
      showAuthFailure(
        attempt,
        registerError,
        error,
        error instanceof Error ? error.message : 'Registration failed',
      );
    } finally {
      if (authFlow.current(attempt) && !authFlow.isUncertain(attempt)) {
        authFlow.finish(attempt);
        authSessionPending(false);
        clearAuthSecrets();
        registerSubmit.disabled = false;
        registerPasskeyBtn.disabled = false;
        registerSubmit.textContent = 'Create Account';
      }
    }
  }, 'Could not complete registration'),
);

async function submitPasskey(registration: boolean): Promise<void> {
  const email = (registration ? registerEmail : loginEmail).value.trim();
  const displayName = registerName.value.trim();
  const errorNode = registration ? registerError : loginError;
  const passkeyButton = registration ? registerPasskeyBtn : loginPasskeyBtn;
  const passwordButton = registration ? registerSubmit : loginSubmit;
  if (registration && (!email || !displayName)) {
    errorNode.textContent = 'Fill in email and display name first';
    errorNode.hidden = false;
    return;
  }
  const attempt = authFlow.begin(false);
  if (!attempt) return;
  errorNode.hidden = true;
  passkeyButton.disabled = true;
  passwordButton.disabled = true;
  passkeyButton.textContent = 'Waiting for passkey...';
  let completed = false;
  try {
    let credential: Credential | null;
    if (registration) {
      const options = await telemetry.measure('passkey_register_start', () =>
        auth.passkeyRegisterStart(email, displayName, attempt.controller.signal),
      );
      if (!authFlow.current(attempt)) return;
      credential = await telemetry.measure('passkey_register_ceremony', async () => {
        const result = await navigator.credentials.create({
          ...options,
          signal: attempt.controller.signal,
        });
        if (!result) throw new DOMException('Passkey cancelled or timed out', 'NotAllowedError');
        return result;
      });
    } else {
      const options = await telemetry.measure('passkey_login_start', () =>
        auth.passkeyLoginStart(attempt.controller.signal),
      );
      if (!authFlow.current(attempt)) return;
      credential = await telemetry.measure('passkey_login_ceremony', async () => {
        const result = await navigator.credentials.get({
          ...options,
          signal: attempt.controller.signal,
        });
        if (!result) throw new DOMException('Passkey cancelled or timed out', 'NotAllowedError');
        return result;
      });
    }
    if (!authFlow.current(attempt)) return;
    if (!credential) throw new DOMException('Passkey cancelled or timed out', 'NotAllowedError');
    if (!authFlow.establish(attempt)) return;
    authSessionPending(true);
    passkeyButton.textContent = registration ? 'Creating account...' : 'Signing in...';
    if (registration)
      await telemetry.measure('passkey_register_finish', () =>
        auth.passkeyRegisterFinish(credential),
      );
    else await telemetry.measure('passkey_login_finish', () => auth.passkeyLoginFinish(credential));
    if (authFlow.finish(attempt)) {
      completed = true;
      dismissAuth();
      loginEmail.value = '';
      registerEmail.value = '';
      registerName.value = '';
    }
  } catch (error) {
    showAuthFailure(
      attempt,
      errorNode,
      error,
      passkeyErrorMessage(
        error,
        registration ? 'Passkey registration failed' : 'Passkey sign-in failed',
      ),
    );
  } finally {
    if (!authFlow.isUncertain(attempt) && (authFlow.current(attempt) || completed)) {
      authFlow.finish(attempt);
      authSessionPending(false);
      clearAuthSecrets();
      passkeyButton.disabled = false;
      passwordButton.disabled = false;
      passkeyButton.textContent = registration ? 'Register with passkey' : 'Use a passkey';
    }
  }
}
loginPasskeyBtn.addEventListener(
  'click',
  asyncUiAction(() => submitPasskey(false), 'Could not complete passkey sign-in'),
);
registerPasskeyBtn.addEventListener(
  'click',
  asyncUiAction(() => submitPasskey(true), 'Could not complete passkey registration'),
);

function passkeyErrorMessage(error: unknown, fallback: string): string {
  if (error instanceof Error && (error.name === 'NotAllowedError' || error.name === 'AbortError'))
    return 'Passkey request cancelled or timed out. You can try again. See Passkey help if your password manager did not appear.';
  return error instanceof Error ? error.message : fallback;
}

function dismissCreateRoom(): void {
  createRoomAttempt++;
  createRoomModal.hidden = true;
  createRoomModal.close();
  crPassword.value = '';
  createRoomError.hidden = true;
  createRoomError.textContent = '';
}

/** Owned, masked, disposable prompt; closing resolves cancellation exactly once. */
function requestRoomPassword(owner: RoomClient, reconnecting = false): Promise<string | null> {
  if (room !== owner) return Promise.resolve(null);
  roomPasswordView?.cancel();
  const membership = owner.membershipVersion;
  const view = modal(reconnecting ? 'Reconnect to room' : 'Room password');
  const field = el('input');
  field.type = 'password';
  field.autocomplete = 'current-password';
  field.maxLength = 256;
  field.id = 'join-room-password';
  const label = el('label', 'Room password');
  label.htmlFor = field.id;
  const form = el('form');
  const submit = el('button', reconnecting ? 'Reconnect' : 'Join room', 'btn-primary');
  submit.type = 'submit';
  form.append(label, field, submit);
  view.body.append(form);
  field.focus();
  const ownedView = {
    cancel: (): void => {
      field.value = '';
      view.close();
    },
  };
  roomPasswordView = ownedView;
  return new Promise((resolve) => {
    let answer: string | null = null;
    form.addEventListener('submit', (event) => {
      event.preventDefault();
      answer = field.value;
      field.value = '';
      view.close();
    });
    view.dialog.addEventListener(
      'close',
      () => {
        field.value = '';
        if (roomPasswordView === ownedView) roomPasswordView = null;
        resolve(room === owner && owner.membershipVersion === membership ? answer : null);
      },
      { once: true },
    );
  });
}

/** A password prompt belongs to the exact room membership that requested it. */
async function joinRoomWithPassword(
  owner: RoomClient,
  roomId: string,
  name: string,
): Promise<'joined' | 'lobby' | null> {
  try {
    return await owner.join(roomId, name);
  } catch (error) {
    if (room !== owner) return null;
    if (!(error instanceof RoomPasswordRequiredError)) throw error;
    const membership = owner.membershipVersion;
    const password = await requestRoomPassword(owner);
    if (room !== owner || owner.membershipVersion !== membership) return null;
    if (password === null) {
      await leaveCurrentRoom();
      return null;
    }
    return owner.join(roomId, name, password);
  }
}

// --- Create Room ---
createRoomBtn.addEventListener('click', () => {
  if (!auth.isLoggedIn || !capabilities?.roomCreation) return;
  createRoomAttempt++;
  createRoomModal.hidden = false;
  createRoomModal.showModal();
  crName.focus();
});
createRoomClose.addEventListener('click', dismissCreateRoom);
createRoomModal.addEventListener('click', (e) => {
  if (e.target === createRoomModal) dismissCreateRoom();
});

/** Room IDs appear in links: ASCII letters, digits and dashes derived from the name. */
function suggestRoomId(displayName: string): string {
  return displayName
    .normalize('NFKD')
    .replace(/[\u0300-\u036f]/g, '')
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 64)
    .replace(/-+$/g, '');
}

let roomIdEdited = false;
crId.addEventListener('input', () => {
  roomIdEdited = crId.value !== '';
});
crName.addEventListener('input', () => {
  if (!roomIdEdited) crId.value = suggestRoomId(crName.value);
});

createRoomSubmit.addEventListener(
  'click',
  asyncUiAction(async () => {
    createRoomError.hidden = true;
    const id = crId.value.trim();
    const displayName = crName.value.trim();
    if (!id || !displayName) {
      createRoomError.textContent = 'A room name and a room ID are required';
      createRoomError.hidden = false;
      return;
    }
    const roomPasswordBytes = new TextEncoder().encode(crPassword.value).length;
    if (crPassword.value && (roomPasswordBytes < 8 || roomPasswordBytes > 256)) {
      createRoomError.textContent = 'Room password must be 8-256 bytes';
      createRoomError.hidden = false;
      return;
    }
    if (createRoomSubmit.disabled) return;
    const attempt = ++createRoomAttempt;
    const mutation = ++createRoomMutation;
    const account = auth.userId;
    const selection = roomSelectionVersion;
    const current = (): boolean =>
      attempt === createRoomAttempt &&
      createRoomModal.open &&
      auth.userId === account &&
      selection === roomSelectionVersion;
    let uncertain = false;
    createRoomSubmit.disabled = true;
    createRoomSubmit.textContent = 'Creating...';
    try {
      const body: CreateRoomRequest = {
        id,
        display_name: displayName,
      };
      if (crTopic.value.trim()) body.topic = crTopic.value.trim();
      if (crPassword.value) body.password = crPassword.value;
      if (crModerated.checked) body.moderated = true;
      if (crLobby.checked) body.lobby_enabled = true;
      if (crSecret.checked) body.secret = true;
      if (!crGuests.checked) body.guests_allowed = false;

      await api.createRoom(auth.jwt, body);
      if (!current()) {
        if (auth.userId === account) showToast('Room created. Open it from My rooms when ready.');
        return;
      }
      dismissCreateRoom();
      // Auto-join the created room
      roomInput.value = id;
      if (auth.displayName && !nameInput.value.trim()) {
        nameInput.value = auth.displayName;
      }
      updateJoinBtn();
      joinBtn.click();
      // Reset form
      crId.value = '';
      crName.value = '';
      crTopic.value = '';
      crPassword.value = '';
      crModerated.checked = false;
      crLobby.checked = false;
      crSecret.checked = false;
      crGuests.checked = true;
      roomIdEdited = false;
    } catch (e) {
      uncertain = e instanceof ApiOutcomeUnknownError;
      if (!current()) {
        if (e instanceof ApiOutcomeUnknownError && auth.userId === account)
          showToast(e.message, 8000, 'error');
        return;
      }
      createRoomError.textContent = e instanceof Error ? e.message : 'Failed to create room';
      createRoomError.hidden = false;
    } finally {
      if (mutation === createRoomMutation) {
        createRoomSubmit.disabled = uncertain;
        createRoomSubmit.textContent = uncertain ? 'Response not confirmed' : 'Create Room';
      }
    }
  }, 'Could not complete room creation'),
);

joinBtn.addEventListener(
  'click',
  asyncUiAction(async () => {
    const name = nameInput.value.trim();
    const roomId = roomInput.value.trim();
    if (
      !name ||
      !roomId ||
      room ||
      departureInProgress ||
      navigationPending ||
      !navigation.join(roomId)
    )
      return;

    writeLocalPreference('displayName', name);

    joinBtn.disabled = true;
    joinBtn.textContent = 'Joining...';
    localTextMuted = false;
    roomRecovering = false;

    let joiningRoom: RoomClient | null = null;
    try {
      roomPasswordView?.cancel();
      room = new RoomClient(signaling, {
        onTelemetry: telemetry.record,
        onCallSignal: (signal) => {
          if (room === joiningRoom) callTelemetry.signal(signal);
        },
        onBackgroundError: (message) => showToast(message),
        onParticipantsChanged: (participants) => {
          community.refresh();
          observeUiTask(socialChat.activate(), 'Could not refresh room conversations');
          renderParticipants(participants);
          socialChat.participantsChanged();
        },
        onLocalStream: () => {}, // Unused — local tile managed by updateLocalTile on user action
        onLocalMediaChanged: () => {
          if (!room) return;
          updateMicButton(room.audioEnabled);
          updateCamButton(room.videoEnabled);
          updateScreenButton(room.isScreenSharing);
          updateLocalTile();
        },
        onLocalCaptureStopped: handleLocalCaptureStopped,
        onLocalVideoStalled: handleLocalVideoStalled,
        onRemoteTrack: renderRemoteTrack,
        onRemoteTrackRemoved: removeRemoteTrack,
        onRemoteMediaUnavailable: (_participantId, participantName, kind, _source, reason) => {
          showToast(`Cannot receive ${participantName}'s ${kind}: ${reason}`, 6000);
        },
        onParticipantLeft: handleParticipantLeft,
        onChatMessage: () => {}, // Typed entries and delivery state handled by session inbox.
        onSocialEvent: (message) => {
          socialChat.handleEvent(message);
          if (message.type === 'nicknameChanged') {
            if (message.participantId === room?.localParticipantId)
              nameInput.value = message.nickname;
            for (const [key, tile] of remoteTiles) {
              if (tile.dataset['participantId'] !== message.participantId) continue;
              const tag = tile.querySelector('.name-tag');
              if (tag)
                tag.textContent = message.nickname + (key.endsWith(':screen') ? ' (Screen)' : '');
            }
            // An automatic color follows the name.
            repaintTiles(message.participantId);
          } else if (message.type === 'chatStyleChanged') {
            repaintTiles(message.participantId);
          } else if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot') {
            const lobby = message.data['lobby'] as
              { participantId: string; displayName: string }[] | undefined;
            lobbyWaiters.clear();
            for (const waiter of lobby ?? [])
              lobbyWaiters.set(waiter.participantId, waiter.displayName);
            renderLobbyPanel();
          }
        },
        onConnectionQuality: renderConnectionQuality,
        onParticipantJoined: handleParticipantJoined,
        onActiveSpeaker: (participantId) => {
          if (currentDominantTile) {
            currentDominantTile.classList.remove('dominant-speaker');
            currentDominantTile = null;
          }
          const tile = remoteTiles.get(participantId);
          if (tile) {
            tile.classList.add('dominant-speaker');
            currentDominantTile = tile;
          }
        },
        onAudioLevels: (levels) => {
          if (levels.length === 0) {
            clearSpeakingHighlights();
            return;
          }
          if (speakingHighlightTimer !== null) clearTimeout(speakingHighlightTimer);
          speakingHighlightTimer = setTimeout(
            clearSpeakingHighlights,
            SPEAKING_HIGHLIGHT_TIMEOUT_MS,
          );
          // Clear only previously-speaking elements (O(n) on speakers, not DOM)
          for (const el of currentlySpeaking) {
            el.classList.remove('speaking');
          }
          currentlySpeaking.clear();
          for (const { participantId, volume } of levels) {
            if (volume > AUDIO_LEVEL_THRESHOLD) {
              const tile = remoteTiles.get(participantId);
              if (tile) {
                tile.classList.add('speaking');
                currentlySpeaking.add(tile);
              }
              // Also update participant list items (cached by data attribute)
              const listItem = participantList.querySelector<HTMLElement>(
                `[data-participant-id="${CSS.escape(participantId)}"]`,
              );
              if (listItem) {
                listItem.classList.add('speaking');
                currentlySpeaking.add(listItem);
              }
              // Classic users panel
              const classicItem = document
                .getElementById('classic-users-panel')
                ?.querySelector<HTMLElement>(
                  `[data-participant-id="${CSS.escape(participantId)}"]`,
                );
              if (classicItem) {
                classicItem.classList.add('speaking');
                currentlySpeaking.add(classicItem);
              }
            }
          }
        },
        onModeration: (action, participantId, reason) => {
          const isLocal = participantId === room?.localParticipantId;
          if (isLocal && (action === 'kicked' || action === 'banned')) {
            observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
            showRoomExitNotice(
              'Removed from the room',
              `You were ${action} from this room.${reason ? ` Reason: ${reason}` : ''}`,
            );
            return;
          }
          if (isLocal && (action === 'textMuted' || action === 'textUnmuted')) {
            localTextMuted = action === 'textMuted';
            applyRoomSettingsToUI();
          }
          // For other participants, show toast as before
          const participants = room?.getParticipants();
          const targetName = participants?.get(participantId)?.name ?? participantId.slice(0, 8);
          const reasonText = reason ? ` (${reason})` : '';
          const messages: Record<string, string> = {
            camBanned: `${targetName} was camera banned${reasonText}`,
            camUnbanned: `${targetName} was camera unbanned`,
            textMuted: `${targetName} was text muted`,
            textUnmuted: `${targetName} was text unmuted`,
            kicked: `${targetName} was kicked${reasonText}`,
            banned: `${targetName} was banned${reasonText}`,
          };
          showToast(messages[action] ?? `${action} on ${targetName}`);
        },
        onRoleChanged: (participantId, newRole) => {
          community.refresh();
          socialChat.participantsChanged();
          const participants = room?.getParticipants();
          const targetName =
            participants?.get(participantId)?.name ??
            (participantId === room?.localParticipantId
              ? room.nickname
              : participantId.slice(0, 8));
          if (participantId === room?.localParticipantId) {
            showToast(`Your role has been changed to ${newRole}`);
            if (newRole !== 'owner' && newRole !== 'admin') roomSettingsModal.close();
            handBtn.classList.remove('hand-raised');
          } else {
            showToast(`${targetName} is now ${newRole}`);
          }
          updateRoomModeUI();
          applyRoomSettingsToUI();
          // Re-render participants to update role badges
          if (room) renderParticipants(room.getParticipants());
        },
        onRoomSettingsChanged: (settings) => {
          roomLabel.textContent = settings.displayName;
          applyAppearance(roomLabel, settings.nameStyle, settings.displayName);
          applyAppearance(roomTopic, settings.topicStyle, settings.topic ?? '');
          socialChat.participantsChanged();
          updateRoomModeUI();
          applyRoomSettingsToUI();
          roomTopic.textContent = settings.topic ?? '';
          roomTopic.hidden = !settings.topic;
        },
        onTopicChanged: (topic, changedBy) => {
          roomTopic.textContent = topic;
          applyAppearance(roomTopic, room?.roomSettings?.topicStyle, topic);
          roomTopic.hidden = !topic;
          showToast(`Topic changed by ${changedBy}: ${topic}`);
        },
        onVoiceRequested: (participantId, displayName) => {
          const role = room?.role ?? 'user';
          const canGrant = role === 'owner' || role === 'admin' || role === 'moderator';
          if (canGrant) {
            showActionToast(`${displayName} is requesting voice`, [
              {
                label: 'Grant',
                action: () => {
                  const owner = room;
                  if (owner)
                    observeUiTask(owner.setRole(participantId, 2), 'Could not grant voice');
                },
              },
              { label: 'Dismiss', action: () => {} },
            ]);
          } else {
            showToast(`${displayName} is requesting voice`);
          }
        },
        onLobbyWaiting: (roomName, topic, count, moderators) => {
          mediaControls.reset();
          setRoomToolsVisible(false);
          joinScreen.hidden = true;
          roomScreen.hidden = true;
          lobbyScreen.hidden = false;
          lobbyRoomName.textContent = roomName;
          lobbyTopic.textContent = topic ?? '';
          lobbyTopic.hidden = !topic;
          updateLobbyStatus(count, moderators);
        },
        onLobbyStatus: updateLobbyStatus,
        onLobbyJoin: (participantId, displayName) => {
          lobbyWaiters.set(participantId, displayName);
          renderLobbyPanel();
          showToast(`${displayName} is waiting in the lobby`);
        },
        onLobbyAdmitted: () => {
          lobbyScreen.hidden = true;
          joinScreen.hidden = true;
          roomScreen.hidden = false;
          applyJoinedRoomUI();
          announceRoomEntry(room?.roomSettings?.displayName ?? room?.currentRoomId ?? 'the room');
        },
        onAdmissionComplete: () => {
          // Post-admission media + room state are ready — refresh buttons/settings UI
          applyJoinedRoomUI();
        },
        onLobbyDenied: (reason) => {
          observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
          showRoomExitNotice(
            'Entry declined',
            `A moderator declined your request to enter.${reason ? ` ${reason}` : ''}`,
          );
        },
        onRecoveryState: (state, message) => {
          if (state === 'reconnecting') socialChat.prepareSnapshotRecovery();
          document.getElementById('room-recovery-notice')?.remove();
          roomRecovering = state !== 'connected';
          if (state !== 'connected') retireRoomSettingsAction();
          if (state === 'connected') {
            connectionStatus.textContent = 'Connected';
            connectionStatus.className = 'status connected';
            if (room?.localParticipantId) {
              applyJoinedRoomUI();
              updateLocalTile();
            }
            showToast(message ?? 'Room connection restored');
          } else {
            connectionStatus.textContent =
              state === 'reconnecting' ? 'Rejoining room…' : 'Room recovery failed';
            connectionStatus.className = `status ${state === 'reconnecting' ? 'connecting' : 'disconnected'}`;
            pttDeactivate();
            applyRoomSettingsToUI();
            if (state === 'failed') {
              const recoveringRoom = room;
              const notice = showActionToast(
                message ?? 'Unable to rejoin the room',
                [
                  {
                    label: 'Retry connection',
                    action: () => {
                      if (room === recoveringRoom) recoveringRoom?.retryRecovery();
                    },
                  },
                  {
                    label: 'Leave room',
                    action: () => {
                      if (room === recoveringRoom)
                        observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
                    },
                  },
                ],
                0,
              );
              notice.id = 'room-recovery-notice';
            }
          }
        },
        onPasswordRequired: () =>
          joiningRoom ? requestRoomPassword(joiningRoom, true) : Promise.resolve(null),
        onRoomClosed: (reason) => {
          observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
          showToast(reason, 8000);
        },
      });

      room.preferChatStyle(socialChat.savedLook());
      joiningRoom = room;
      const status = await joinRoomWithPassword(joiningRoom, roomId, name);
      if (room !== joiningRoom || status === null) return;

      if (status === 'lobby') {
        // onLobbyWaiting already switched to the lobby screen. Room UI is applied
        // by onLobbyAdmitted/onAdmissionComplete if/when we are admitted.
        return;
      }

      joinScreen.hidden = true;
      roomScreen.hidden = false;
      applyJoinedRoomUI();
      announceRoomEntry(joiningRoom.roomSettings?.displayName ?? roomId);
    } catch (e) {
      if (room !== joiningRoom) return;
      await leaveCurrentRoom();
      if (room) return;
      reportJoinFailure(e);
    }
  }, 'Could not complete joining the room'),
);

function reportJoinFailure(error: unknown): void {
  console.error('Failed to join:', error);
  showRoomExitNotice('Could not join', error instanceof Error ? error.message : String(error));
  joinBtn.textContent = 'Join Room';
  updateJoinBtn();
}

/** Apply all in-room UI state (label, topic, control buttons, settings-driven UI).
 * Called on direct join, on lobby admission (media pending), and again once
 * post-admission media setup completes. Idempotent. */
function applyJoinedRoomUI(): void {
  if (!room) return;
  setLayout(getLayout());

  roomLabel.textContent = room.roomSettings?.displayName ?? room.currentRoomId ?? '';
  applyAppearance(roomLabel, room.roomSettings?.nameStyle, roomLabel.textContent);
  applyAppearance(roomTopic, room.roomSettings?.topicStyle, room.roomSettings?.topic ?? '');
  roomLabel.hidden = false;
  setRoomToolsVisible(true);
  placeDiagnosticsButton(true);

  const topic = room.roomSettings?.topic;
  roomTopic.textContent = topic ?? '';
  roomTopic.hidden = !topic;

  // Reflect retained media on reconnect; fresh room sessions start without capture.
  if (room.hasMedia) {
    micBtn.style.opacity = '';
    camBtn.style.opacity = '';
    micBtn.classList.remove('muted');
    camBtn.classList.remove('muted');
    updateMicButton(room.audioEnabled);
    updateCamButton(room.videoEnabled);
    updateScreenButton(room.isScreenSharing);
    // Register callback so UI updates when browser's "Stop sharing" button is clicked
    const owner = room;
    const membership = owner.membershipVersion;
    room.onScreenShareStopped = () => {
      if (room === owner && owner.membershipVersion === membership) updateScreenButton(false);
    };
    room.onScreenShareAudioChanged = () => {
      if (room === owner && owner.membershipVersion === membership)
        updateScreenButton(owner.isScreenSharing);
    };
  } else {
    // Media unavailable (or not yet set up while waiting in lobby)
    console.warn('[ui] media not available, buttons will be non-functional');
    setButtonContent(micBtn, icons.micOff(), 'No mic access');
    micBtn.classList.add('muted');
    micBtn.style.opacity = '0.4';
    setButtonContent(camBtn, icons.camOff(), 'No cam access');
    camBtn.classList.add('muted');
    camBtn.style.opacity = '0.4';
  }

  qualityIndicator.hidden = false;
  updateRoomModeUI();
  applyRoomSettingsToUI();
  observeUiTask(socialChat.activate(), 'Could not refresh room conversations');
  community.refresh();
}

let roomTopicView: ReturnType<typeof modal> | null = null;

// Every participant can read the full topic; editing still uses room permissions.
roomTopic.addEventListener('click', () => {
  if (!room || !roomTopic.textContent) return;
  const activeRoom = room;
  const membership = room.membershipVersion;
  roomTopicView?.close();
  const view = modal('Room topic');
  roomTopicView = view;
  view.dialog.addEventListener(
    'close',
    () => {
      if (roomTopicView === view) roomTopicView = null;
    },
    { once: true },
  );
  view.body.append(el('p', roomTopic.textContent, 'room-topic-full'));
  if (room.role === 'owner' || room.role === 'admin') {
    view.body.append(
      button('Edit topic', () => {
        view.close();
        if (
          room === activeRoom &&
          room.membershipVersion === membership &&
          (room.role === 'owner' || room.role === 'admin')
        )
          roomSettingsBtn.click();
      }),
    );
  }
});

function updateLobbyStatus(count: number, moderators: number): void {
  lobbyCount.textContent = `${count} connected participant${count !== 1 ? 's' : ''} in room · ${moderators > 0 ? `${moderators} moderator${moderators !== 1 ? 's' : ''} connected` : 'No moderator is currently connected'}. ${moderators > 0 ? 'Your request is waiting for approval.' : 'You can wait here or return home; a moderator must join to admit you.'}`;
  lobbyCount.setAttribute('role', 'status');
}

// --- Lobby Cancel ---
lobbyCancelBtn.addEventListener('click', () => navigation.home());

// --- Leave ---

function leaveCurrentRoom(): Promise<void> {
  departureInProgress ??= leaveRoomAndShowHome().finally(() => {
    departureInProgress = null;
    updateJoinBtn();
    navigation.resumePendingJoin();
  });
  updateJoinBtn();
  return departureInProgress;
}

async function leaveRoomAndShowHome(): Promise<void> {
  participantHovercard.reset();
  retireRoomSettingsAction();
  roomPasswordView?.cancel();
  roomPasswordView = null;
  roomTopicView?.close();
  document.getElementById('room-recovery-notice')?.remove();
  pttDeactivate();
  mediaControls.reset();
  const leavingRoom = room;
  room = null;
  socialChat.reset();
  community.refresh();
  await leavingRoom?.leave();
  // An explicit departure also makes the homepage usable after a bounded
  // recovery failure, without changing the retained account identity.
  if (signaling.reconnectExhausted) signaling.retryConnection();
  localTextMuted = false;
  roomRecovering = false;
  setMicMode(personalMicMode, false);
  micModeSelect.disabled = false;

  clearChildren(videoGrid);
  clearChildren(participantList);
  clearChildren(chatMessages);
  unreadBadge.hidden = true;
  scrollBottomBtn.hidden = true;
  remoteTiles.clear();
  setPinnedTile(null);
  lobbyWaiters.clear();
  lobbyActions.clear();

  // Remove classic users panel if present
  document.getElementById('classic-users-panel')?.remove();

  // Reset speaking/active-speaker tracking
  clearSpeakingHighlights();

  // Reset PTT state
  pttHeld = false;
  pttActivation++;

  // Reset mic/cam button states (clear inline opacity from no-media mode)
  micBtn.style.opacity = '';
  camBtn.style.opacity = '';
  micBtn.classList.remove('active', 'muted', 'ptt-active');
  camBtn.classList.remove('active', 'muted');
  updateScreenButton(false);
  handBtn.hidden = true;
  handBtn.classList.remove('hand-raised');
  roomSettingsBtn.hidden = true;
  setButtonContent(micBtn, icons.micOn(), 'Mic (M)');
  setButtonContent(camBtn, icons.camOn(), 'Cam (V)');
  setButtonContent(screenBtn, icons.screenShare(), 'Screen (S)');
  // Remove PTT label if present
  micBtn.querySelector('.ptt-label')?.remove();

  // Reset settings-driven UI
  chatInput.disabled = false;
  (chatSendBtn as HTMLButtonElement).disabled = false;
  chatInput.placeholder = 'Type a message...';
  chatSendBtn.classList.remove('disabled');
  screenBtn.hidden = !navigator.mediaDevices?.getDisplayMedia;
  camBtn.hidden = false;

  roomScreen.hidden = true;
  setRoomToolsVisible(false);
  lobbyScreen.hidden = true;
  joinScreen.hidden = false;
  placeDiagnosticsButton(false);
  roomLabel.hidden = true;
  roomTopic.hidden = true;
  roomTopic.textContent = '';
  joinBtn.textContent = 'Join Room';
  qualityIndicator.hidden = true;
  qualityIndicator.className = 'quality-dot';
  roomSettingsModal.close();
  document.getElementById('mod-menu')?.remove();
  updateJoinBtn();
  observeUiTask(loadRoomBrowser(), 'Could not refresh the room directory');
}

leaveBtn.addEventListener('click', () => navigation.home());

homeLink.addEventListener('click', (event) => {
  if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)
    return;
  event.preventDefault();
  navigation.home();
  homeLink.focus();
});

// --- Control buttons ---
function updateMicButton(enabled: boolean): void {
  let tooltip: string;
  if (micMode === 'ptt') {
    tooltip = enabled ? 'Release to mute' : 'Hold Space/T to talk';
  } else {
    tooltip = enabled ? 'Turn off mic (M)' : 'Turn on mic (M)';
  }
  setButtonContent(micBtn, enabled ? icons.micOn() : icons.micOff(), tooltip);
  micBtn.classList.toggle('active', enabled);
  micBtn.classList.toggle('muted', !enabled);
  micBtn.classList.toggle('ptt-active', micMode === 'ptt' && enabled);

  // PTT mode label under mic button
  let pttLabel = micBtn.querySelector('.ptt-label');
  if (micMode === 'ptt') {
    if (!pttLabel) {
      pttLabel = document.createElement('span');
      pttLabel.className = 'ptt-label';
      micBtn.appendChild(pttLabel);
    }
    pttLabel.textContent = 'PTT';
  } else {
    pttLabel?.remove();
  }
}

function updateCamButton(enabled: boolean): void {
  setButtonContent(
    camBtn,
    enabled ? icons.camOn() : icons.camOff(),
    enabled ? 'Turn off camera (V)' : 'Turn on camera (V)',
  );
  camBtn.classList.toggle('active', enabled);
  camBtn.classList.toggle('muted', !enabled);
}

function screenAudioLabel(): string {
  switch (room?.screenShareAudio) {
    case 'sharing':
      return 'screen audio included';
    case 'failed':
      return 'screen audio failed';
    case 'ended':
      return 'screen audio ended';
    case undefined:
    case 'off':
    case 'not_provided':
      return 'no screen audio';
  }
}

function updateScreenButton(active: boolean): void {
  setButtonContent(
    screenBtn,
    active ? icons.screenShareOff() : icons.screenShare(),
    active ? `Stop Sharing (S) — ${screenAudioLabel()}` : 'Screen (S)',
  );
  screenBtn.classList.toggle('active', active);
  screenShareStatus.hidden = !active;
  screenShareStatus.textContent = active ? `Sharing your screen · ${screenAudioLabel()}` : '';
}

/** Show/hide/update the local video tile based on current mic+cam state */
function updateLocalTile(): void {
  if (!room) return;
  const camOn = room.videoEnabled;
  const micOn = room.audioEnabled;
  const tile = document.getElementById('local-tile');
  const localName = room.nickname || nameInput.value.trim();

  if (!camOn && !micOn) {
    // Both off — remove tile entirely
    tile?.remove();
    updateVideoGridCount();
    return;
  }

  if (!tile) {
    // Need to re-create the tile (was removed when both were off)
    const newTile = document.createElement('div');
    newTile.className = 'video-tile local';
    newTile.id = 'local-tile';

    const nameTag = document.createElement('div');
    nameTag.className = 'name-tag';
    nameTag.textContent = `${localName} (You)`;
    newTile.appendChild(nameTag);
    paintTile(newTile, localName, room.chatStyle?.color ?? null);

    videoGrid.prepend(newTile);
    updateVideoGridCount();

    if (camOn) {
      // Re-attach video from local stream
      const stream = room.getLocalStream();
      if (stream) {
        const video = document.createElement('video');
        video.autoplay = true;
        video.muted = true;
        video.playsInline = true;
        video.srcObject = stream;
        newTile.insertBefore(video, newTile.firstChild);
      }
    } else {
      // Mic on, cam off — show avatar
      addLocalAvatar(newTile, localName);
    }
    return;
  }

  // Tile exists — update it
  if (camOn) {
    // Show video, hide avatar
    const avatar = tile.querySelector('.no-video-avatar') as HTMLElement | null;
    if (avatar) avatar.remove();
    if (!tile.querySelector('video')) {
      const stream = room.getLocalStream();
      if (stream) {
        const video = document.createElement('video');
        video.autoplay = true;
        video.muted = true;
        video.playsInline = true;
        video.srcObject = stream;
        tile.insertBefore(video, tile.firstChild);
      }
    }
  } else {
    // Cam off — remove video, show avatar
    const video = tile.querySelector('video');
    if (video) {
      video.srcObject = null;
      video.remove();
    }
    if (!tile.querySelector('.no-video-avatar')) {
      addLocalAvatar(tile, localName);
    }
  }
}

function addLocalAvatar(tile: HTMLElement, name: string): void {
  const noVideoAvatar = document.createElement('div');
  noVideoAvatar.className = 'no-video-avatar';
  const initial = document.createElement('div');
  initial.className = 'avatar-initial';
  initial.textContent = name.charAt(0).toUpperCase();
  noVideoAvatar.appendChild(initial);
  tile.insertBefore(noVideoAvatar, tile.firstChild);
  paintTile(tile, name, room?.chatStyle?.color ?? null);
}

/** A person's chosen palette color, the local person included; null means automatic. */
function participantColor(participantId: string): string | null {
  if (!room) return null;
  const look =
    participantId === room.localParticipantId
      ? room.chatStyle
      : room.getParticipants().get(participantId)?.chatStyle;
  return look?.color ?? null;
}

/** A person's color on a tile: the dot beside the name tag and the camera-off initial. */
function paintTile(tile: HTMLElement, name: string, color: string | null): void {
  tile.style.setProperty('--person-color', chatColor(name, color));
  const initial = tile.querySelector<HTMLElement>('.avatar-initial');
  if (initial) Object.assign(initial.style, avatarColors(name, color));
}

/** A changed look or name repaints that person's tiles; the people lists re-render themselves. */
function repaintTiles(participantId: string): void {
  if (!room) return;
  if (participantId === room.localParticipantId) {
    const tile = document.getElementById('local-tile');
    if (tile)
      paintTile(tile, room.nickname || nameInput.value.trim(), participantColor(participantId));
    return;
  }
  const name = room.getParticipants().get(participantId)?.name;
  if (name === undefined) return;
  for (const tile of remoteTiles.values())
    if (tile.dataset['participantId'] === participantId)
      paintTile(tile, name, participantColor(participantId));
}

function handleLocalCaptureStopped(kind: 'audio' | 'video'): void {
  if (!room) return;
  if (kind === 'audio') {
    // Retire the held intent and any pending activation; only a new user action may recapture.
    pttActivation++;
    pttHeld = false;
    showToast(
      micMode === 'ptt'
        ? 'Microphone stopped. Release, then hold Space/T or the microphone button again to restart.'
        : 'Microphone stopped. Press M or the mic button to turn it back on.',
    );
  } else {
    showToast('Camera stopped. Press V or the camera button to turn it back on.');
  }
}

/** The broadcast stays on, so viewers see it resume with the camera. */
function handleLocalVideoStalled(stalled: boolean): void {
  if (stalled)
    showToast(
      'Your camera is not sending video. Check that it is connected and not in use by another app; your broadcast resumes when it does.',
      10_000,
      'error',
    );
  else showToast('Your camera is sending video again.');
}

async function pttActivate(): Promise<void> {
  if (!room || !room.hasMedia || pttHeld) return;
  const activeRoom = room;
  const membership = activeRoom.membershipVersion;
  const activation = ++pttActivation;
  pttHeld = true;
  try {
    await activeRoom.unmuteAudio();
    if (
      activation !== pttActivation ||
      room !== activeRoom ||
      membership !== activeRoom.membershipVersion ||
      !pttHeld
    )
      return;
    updateMicButton(activeRoom.audioEnabled);
    updateLocalTile();
  } catch (error) {
    if (
      activation !== pttActivation ||
      room !== activeRoom ||
      membership !== activeRoom.membershipVersion ||
      !pttHeld
    )
      return;
    pttHeld = false;
    activeRoom.muteAudio();
    updateMicButton(false);
    updateLocalTile();
    showToast(mediaErrorMessage(error, 'microphone'));
  }
}

function pttDeactivate(): void {
  if (!room || !pttHeld) return;
  pttActivation++;
  pttHeld = false;
  room.muteAudio();
  updateMicButton(false);
  updateLocalTile();
}

micBtn.addEventListener('mousedown', (e) => {
  if (!room || !room.hasMedia) return;
  if (micMode === 'ptt' && canStartBroadcast('microphone')) {
    e.preventDefault(); // prevent focus loss
    observeUiTask(pttActivate(), 'Could not enable push-to-talk');
  }
});

micBtn.addEventListener('mouseup', () => {
  if (micMode === 'ptt') pttDeactivate();
});

micBtn.addEventListener('mouseleave', () => {
  if (micMode === 'ptt') pttDeactivate();
});

micBtn.addEventListener(
  'click',
  asyncUiAction(async () => {
    if (micMode === 'open') await toggleMicrophone();
    // In PTT mode, click is handled by mousedown/mouseup above
  }, 'Could not update the microphone'),
);

function canStartBroadcast(kind: 'microphone' | 'camera' | 'screen'): boolean {
  if (!room?.hasMedia || roomRecovering) return false;
  const settings = room.roomSettings;
  const hasVoice = ['member', 'moderator', 'admin', 'owner'].includes(room.role);
  if (settings?.moderated && !hasVoice) {
    showToast('Raise your hand to request broadcasting permission');
    return false;
  }
  if (settings?.guestsCanBroadcast === false && room.role === 'guest') {
    showToast('Guests cannot broadcast in this room');
    return false;
  }
  if (kind === 'camera' && settings?.allowVideo === false) return false;
  if (
    kind === 'screen' &&
    (settings?.allowScreenSharing === false || !navigator.mediaDevices?.getDisplayMedia)
  )
    return false;
  return true;
}

async function toggleMicrophone(): Promise<void> {
  const activeRoom = room;
  if (!activeRoom?.hasMedia || microphoneTogglePending || micMode !== 'open') return;
  if (!activeRoom.audioEnabled && !canStartBroadcast('microphone')) return;
  const membership = activeRoom.membershipVersion;
  microphoneTogglePending = true;
  try {
    const enabled = await activeRoom.toggleAudio();
    if (room !== activeRoom || membership !== activeRoom.membershipVersion) return;
    updateMicButton(enabled);
    updateLocalTile();
  } catch (error) {
    if (room !== activeRoom || membership !== activeRoom.membershipVersion) return;
    updateMicButton(activeRoom.audioEnabled);
    showToast(mediaErrorMessage(error, 'microphone'));
  } finally {
    microphoneTogglePending = false;
  }
}

async function toggleCamera(): Promise<void> {
  const activeRoom = room;
  if (!activeRoom?.hasMedia || cameraTogglePending) return;
  if (!activeRoom.videoEnabled && !canStartBroadcast('camera')) return;
  const membership = activeRoom.membershipVersion;
  cameraTogglePending = true;
  try {
    const enabled = await activeRoom.toggleVideo();
    if (room !== activeRoom || membership !== activeRoom.membershipVersion) return;
    updateCamButton(enabled);
    updateLocalTile();
  } catch (error) {
    if (room !== activeRoom || membership !== activeRoom.membershipVersion) return;
    updateCamButton(activeRoom.videoEnabled);
    showToast(mediaErrorMessage(error, 'camera'));
  } finally {
    cameraTogglePending = false;
  }
}

const refreshMediaBtn = button(
  'Refresh incoming media',
  () => {
    const owner = room;
    if (!owner?.hasMedia || refreshMediaBtn.disabled) return;
    const membership = owner.membershipVersion;
    refreshMediaBtn.disabled = true;
    refreshMediaBtn.setAttribute('aria-busy', 'true');
    observeUiTask(
      (async () => {
        try {
          await owner.refreshIncomingMedia();
          if (room === owner && owner.membershipVersion === membership)
            showToast(
              'Incoming subscriptions refreshed. Your microphone and camera settings are unchanged.',
            );
        } catch (error) {
          if (room === owner && owner.membershipVersion === membership)
            showToast(
              error instanceof Error ? error.message : 'Incoming media could not be refreshed',
              7000,
            );
        } finally {
          refreshMediaBtn.disabled = false;
          refreshMediaBtn.setAttribute('aria-busy', 'false');
        }
      })(),
      'Could not refresh incoming media',
    );
  },
  'btn-secondary',
);
refreshMediaBtn.id = 'refresh-incoming-media';
document.getElementById('room-more-repair')!.prepend(refreshMediaBtn);

const screenShareStatus = el('span', '', 'settings-description');
screenShareStatus.id = 'screen-share-status';
screenShareStatus.setAttribute('role', 'status');
screenShareStatus.hidden = true;
roomTools.querySelector('.room-navigation')!.appendChild(screenShareStatus);

const screenShareFailure: Record<
  Extract<ScreenShareResult, { status: 'not_started' }>['reason'],
  string
> = {
  cancelled_or_denied:
    'Screen sharing was cancelled or permission was denied. Try again to choose a screen, window, or tab.',
  unavailable: 'Screen sharing is not supported in this browser.',
  not_ready: 'The call is reconnecting. Try sharing when it is ready.',
  busy: 'A screen-sharing request is already in progress.',
  superseded: 'Screen sharing was cancelled because the room changed.',
  not_readable:
    'Your browser could not access that screen. Check screen-recording permission in your system settings.',
  no_source: 'No shareable screen or window was available.',
  invalid_state: 'Return to this tab and click Screen to choose what to share.',
  failed: 'Screen sharing could not start. Try choosing a source again.',
};

async function toggleScreenShare(): Promise<void> {
  const activeRoom = room;
  if (!activeRoom?.hasMedia) return;
  const membership = activeRoom.membershipVersion;
  if (activeRoom.isScreenSharing) {
    activeRoom.stopScreenShare();
    updateScreenButton(false);
  } else if (canStartBroadcast('screen')) {
    const result = await activeRoom.startScreenShare();
    if (room !== activeRoom || membership !== activeRoom.membershipVersion) return;
    updateScreenButton(activeRoom.isScreenSharing);
    if (result.status === 'not_started') showToast(screenShareFailure[result.reason], 7000);
    else if (result.audio === 'failed')
      showToast(
        'Your screen is shared, but its audio could not start. Stop and share again to retry audio.',
        7000,
      );
  }
}

camBtn.addEventListener('click', asyncUiAction(toggleCamera, 'Could not update the camera'));
screenBtn.addEventListener(
  'click',
  asyncUiAction(toggleScreenShare, 'Could not update screen sharing'),
);

micBtn.addEventListener(
  'touchstart',
  (event) => {
    if (micMode !== 'ptt') return;
    event.preventDefault();
    if (canStartBroadcast('microphone'))
      observeUiTask(pttActivate(), 'Could not enable push-to-talk');
  },
  { passive: false },
);
micBtn.addEventListener('touchend', () => pttDeactivate());
micBtn.addEventListener('touchcancel', () => pttDeactivate());

// --- Hand raise ---
let voiceRequestPending = false;
handBtn.addEventListener(
  'click',
  asyncUiAction(async () => {
    const owner = room;
    if (!owner || voiceRequestPending || handBtn.classList.contains('hand-raised')) return;
    const membership = owner.membershipVersion;
    voiceRequestPending = true;
    handBtn.setAttribute('aria-busy', 'true');
    try {
      await owner.requestVoice();
      if (room !== owner || membership !== owner.membershipVersion) return;
      handBtn.classList.add('hand-raised');
      showToast('Voice request sent to moderators');
    } finally {
      voiceRequestPending = false;
      handBtn.setAttribute('aria-busy', 'false');
    }
  }, 'Could not request voice'),
);

// --- Room Settings Modal ---
configureSettingsDialog(roomSettingsModal);
roomSettingsModal.addEventListener('close', () => {
  rsPassword.value = '';
});
roomSettingsBtn.addEventListener('click', () => {
  if (roomSettingsPending) {
    showToast('A room change is still awaiting confirmation');
    return;
  }
  populateRoomSettingsModal();
  roomSettingsModal.showModal();
});

const roomSettingsStatus = el('p', '', 'settings-description');
roomSettingsStatus.id = 'room-settings-result';
roomSettingsStatus.setAttribute('role', 'status');
roomSettingsStatus.hidden = true;
roomSettingsModal.querySelector('.settings-dialog-body')!.prepend(roomSettingsStatus);
const roomSettingsRetry = button('Retry change', () => {}, 'btn-secondary');
roomSettingsRetry.hidden = true;
roomSettingsStatus.after(roomSettingsRetry);
roomSettingsModal.addEventListener('close', () => {
  roomSettingsRetry.hidden = true;
  roomSettingsRetry.onclick = null;
  roomSettingsStatus.textContent = '';
  roomSettingsStatus.hidden = true;
});

function retireRoomSettingsAction(): void {
  roomSettingsAttempt++;
  roomSettingsPending = false;
  roomSettingsModal.setAttribute('aria-busy', 'false');
  for (const field of roomSettingsModal.querySelectorAll<
    HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement
  >('input, select, textarea'))
    field.disabled = false;
  roomSettingsModal.close();
  roomSettingsRetry.hidden = true;
  roomSettingsRetry.onclick = null;
  roomSettingsStatus.textContent = '';
  roomSettingsStatus.hidden = true;
}

async function applyRoomSetting(change: (owner: RoomClient) => Promise<void>): Promise<void> {
  const owner = room;
  if (!owner || roomSettingsPending) return;
  const membership = owner.membershipVersion;
  const attempt = ++roomSettingsAttempt;
  const current = () =>
    attempt === roomSettingsAttempt && room === owner && owner.membershipVersion === membership;
  const fields = roomSettingsModal.querySelectorAll<
    HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement
  >('input, select, textarea');
  const drafts = Array.from(fields, (field) => ({
    field,
    value: field.value,
    checked: field instanceof HTMLInputElement ? field.checked : undefined,
  }));
  roomSettingsPending = true;
  roomSettingsStatus.hidden = false;
  roomSettingsStatus.textContent = 'Saving change…';
  roomSettingsModal.setAttribute('aria-busy', 'true');
  roomSettingsRetry.hidden = true;
  roomSettingsRetry.onclick = null;
  for (const field of fields) field.disabled = true;
  let confirmed = false;
  try {
    await change(owner);
    confirmed = true;
    if (current()) roomSettingsStatus.textContent = 'Change saved';
  } catch (error) {
    if (current()) {
      roomSettingsStatus.textContent =
        error instanceof Error ? error.message : 'The change was not confirmed';
      roomSettingsStatus.scrollIntoView({ block: 'nearest' });
    }
  } finally {
    if (current()) {
      roomSettingsPending = false;
      roomSettingsModal.setAttribute('aria-busy', 'false');
      for (const field of fields) field.disabled = false;
      populateRoomSettingsModal();
      if (!confirmed && roomSettingsModal.open) {
        for (const draft of drafts) {
          draft.field.value = draft.value;
          if (draft.field instanceof HTMLInputElement && draft.checked !== undefined)
            draft.field.checked = draft.checked;
        }
        roomSettingsRetry.hidden = false;
        roomSettingsRetry.onclick = asyncUiAction(
          () => (current() ? applyRoomSetting(change) : Promise.resolve()),
          'Could not save the room setting',
        );
      }
    }
  }
}

// Room settings toggle handlers
type BooleanRoomSetting = {
  [K in keyof RoomSettingsPatch]-?: NonNullable<RoomSettingsPatch[K]> extends boolean ? K : never;
}[keyof RoomSettingsPatch];
const settingsToggles: [HTMLInputElement, BooleanRoomSetting][] = [
  [rsModerated, 'moderated'],
  [rsLobby, 'lobbyEnabled'],
  [rsScreen, 'allowScreenSharing'],
  [rsChat, 'allowChat'],
  [rsGuests, 'guestsAllowed'],
  [rsGuestsBroadcast, 'guestsCanBroadcast'],
  [rsRequireReg, 'requireRegistration'],
  [rsInviteOnly, 'inviteOnly'],
  [rsSecret, 'secret'],
  [rsVideo, 'allowVideo'],
  [rsPtt, 'pushToTalk'],
];

for (const [el, key] of settingsToggles) {
  el.addEventListener(
    'change',
    asyncUiAction(() => {
      const value = el.checked;
      return applyRoomSetting((owner) => owner.updateRoomSettings({ [key]: value }));
    }, 'Could not save the room setting'),
  );
}

rsTopic.addEventListener(
  'change',
  asyncUiAction(() => {
    const topic = rsTopic.value.trim();
    return applyRoomSetting((owner) => owner.setTopic(topic));
  }, 'Could not save the topic'),
);

rsPassword.addEventListener(
  'change',
  asyncUiAction(async () => {
    const password = rsPassword.value;
    const passwordBytes = new TextEncoder().encode(password).length;
    if (password && (passwordBytes < 8 || passwordBytes > 256)) {
      showToast('Room password must be 8-256 bytes');
      rsPassword.focus();
      return;
    }
    await applyRoomSetting((owner) => owner.updateRoomSettings({ password: password || null }));
  }, 'Could not update the room password'),
);
rsPasswordRemove.addEventListener(
  'click',
  asyncUiAction(removeRoomPassword, 'Could not remove the room password'),
);

/** Blank clears a limit; malformed or fractional input must never silently clear it. */
function roomLimit(input: HTMLInputElement): number | null | undefined {
  if (input.validity.badInput) {
    input.reportValidity();
    return undefined;
  }
  if (input.value === '') return null;
  const value = input.valueAsNumber;
  if (
    !input.checkValidity() ||
    !Number.isSafeInteger(value) ||
    value < 0 ||
    value > 4_294_967_295
  ) {
    roomSettingsStatus.hidden = false;
    roomSettingsStatus.textContent =
      'Enter a whole number from 0 to 4294967295, or leave the limit blank.';
    input.focus();
    return undefined;
  }
  return value;
}

rsMaxBroadcasters.addEventListener(
  'change',
  asyncUiAction(async () => {
    const value = roomLimit(rsMaxBroadcasters);
    if (value === undefined) return;
    await applyRoomSetting((owner) => owner.updateRoomSettings({ maxBroadcasters: value }));
  }, 'Could not update the broadcaster limit'),
);

rsMaxParticipants.addEventListener(
  'change',
  asyncUiAction(async () => {
    const value = roomLimit(rsMaxParticipants);
    if (value === undefined) return;
    await applyRoomSetting((owner) => owner.updateRoomSettings({ maxParticipants: value }));
  }, 'Could not update the participant limit'),
);

// --- Personal settings ---
settingsBtn.addEventListener(
  'click',
  asyncUiAction(() => mediaControls.openSetup('settings'), 'Could not open your settings'),
);

layoutSelect.addEventListener('change', () => {
  setLayout(layoutSelect.value as 'modern' | 'classic');
});

// Mic mode setting
micModeSelect.value = micMode;

function setMicMode(mode: MicMode, remember = true): void {
  if (remember && room?.roomSettings?.pushToTalk && mode !== 'ptt') {
    micModeSelect.value = 'ptt';
    showToast('This room requires push to talk');
    return;
  }
  if (remember) {
    personalMicMode = mode;
    writeLocalPreference('micMode', mode);
  }
  const effectiveMode = room?.roomSettings?.pushToTalk ? 'ptt' : mode;
  const changed = micMode !== effectiveMode;
  micMode = effectiveMode;
  micModeSelect.value = effectiveMode;

  if (room && room.hasMedia) {
    if (effectiveMode === 'ptt' && (changed || (!pttHeld && room.audioEnabled))) {
      // Entering PTT mode — mute immediately
      pttHeld = false;
      pttActivation++;
      room.muteAudio();
      updateMicButton(false);
      updateLocalTile();
    } else if (changed) {
      // Entering open mic mode — reset PTT state, leave mic as-is (muted)
      pttHeld = false;
      pttActivation++;
      updateMicButton(room.audioEnabled);
    }
    updateMicButton(room.audioEnabled);
  }
}

micModeSelect.addEventListener('change', () => {
  setMicMode(micModeSelect.value as MicMode);
});

// --- Update video grid count for adaptive sizing ---
/** Pin one tile first and highlight it without resizing any cells. This viewer only. */
function setPinnedTile(key: string | null): void {
  pinnedTileKey = key !== null && remoteTiles.has(key) ? key : null;
  for (const [tileKey, tile] of remoteTiles) {
    const pinned = tileKey === pinnedTileKey;
    tile.classList.toggle('pinned', pinned);
    const pin = tile.querySelector('.tile-pin');
    if (pin) {
      pin.setAttribute('aria-pressed', String(pinned));
      pin.textContent = pinned ? 'Unpin' : 'Pin';
    }
  }
  videoGrid.classList.toggle('has-pinned', pinnedTileKey !== null);
}

function updateVideoGridCount(): void {
  if (pinnedTileKey !== null && !remoteTiles.has(pinnedTileKey)) setPinnedTile(null);
  const count = videoGrid.children.length;
  if (count <= 1) videoGrid.dataset['count'] = '1';
  else if (count === 2) videoGrid.dataset['count'] = '2';
  else if (count <= 4) videoGrid.dataset['count'] = '4';
  else if (count <= 6) videoGrid.dataset['count'] = '6';
  else videoGrid.dataset['count'] = 'many';
}

// --- Rendering ---
/** Reconcile only the visible roster; unchanged rows retain focus and their avatars. */
function renderParticipants(participants: Map<string, Participant>): void {
  participantHovercard.refresh();
  const all = Array.from(participants.values());
  const localId = room?.localParticipantId;
  if (localId && room) {
    const producers = new Map<string, { kind: 'audio' | 'video'; source?: string }>();
    if (room.audioEnabled) producers.set('local-audio', { kind: 'audio' });
    if (room.videoEnabled) producers.set('local-video', { kind: 'video' });
    all.push({
      id: localId,
      name: room.nickname || nameInput.value.trim(),
      role: room.role,
      authenticated: auth.isLoggedIn,
      ...(room.chatStyle && { chatStyle: room.chatStyle }),
      producers,
    });
  }
  sortParticipantRoster(all);
  community.retainProfiles(all.filter((person) => person.authenticated).map((person) => person.id));
  const classic = getLayout() === 'classic' && isDesktopLayout();
  let panel = document.getElementById('classic-users-panel');
  const visible = classic
    ? !panelPreferences.rosterCollapsed
    : document.getElementById('users-panel')!.classList.contains('active') &&
      !(isDesktopLayout() ? panelPreferences.chatCollapsed : isMobilePanelCollapsed());
  if (!visible) {
    clearChildren(participantList);
    if (classic) panel?.querySelector('.classic-user-list')?.replaceChildren();
    else panel?.remove();
    participantHovercard.refresh();
    return;
  }
  let list: HTMLElement = participantList;
  if (classic) {
    clearChildren(participantList);
    if (!panel) {
      panel = el('aside');
      panel.id = 'classic-users-panel';
      panel.append(el('ul', undefined, 'classic-user-list'));
      roomScreen.insertBefore(panel, roomScreen.firstChild);
      attachPanelResize(panel, 'roster');
      applyPanelPreferences();
    }
    panel.setAttribute('aria-label', `People (${all.length})`);
    list = panel.querySelector<HTMLElement>('.classic-user-list')!;
  } else panel?.remove();
  const existing = new Map(
    Array.from(list.children, (node) => [
      (node as HTMLElement).dataset['participantId'],
      node as HTMLElement,
    ]),
  );
  const retained = new Set<HTMLElement>();
  const focusedId =
    document.activeElement?.closest<HTMLElement>('[data-participant-id]')?.dataset['participantId'];
  let position = list.firstChild;
  for (const p of all) {
    const local = p.id === localId;
    const hasAudio = Array.from(p.producers.values()).some((producer) => producer.kind === 'audio');
    const hasVideo = Array.from(p.producers.values()).some((producer) => producer.kind === 'video');
    const fingerprint = JSON.stringify([
      p.name,
      p.role,
      p.chatStyle,
      p.authenticated,
      classic,
      local,
      hasAudio,
      hasVideo,
    ]);
    let row = existing.get(p.id);
    if (!row || row.dataset['rosterState'] !== fingerprint) {
      row = el('li');
      row.dataset['participantId'] = p.id;
      row.dataset['rosterState'] = fingerprint;
      row.style.setProperty('--person-color', chatColor(p.name, p.chatStyle?.color));
      if (!local)
        row.addEventListener('contextmenu', (event) => {
          event.preventDefault();
          showModerationMenu(p.id, p.name, event.clientX, event.clientY);
        });
      const avatar = el('div', p.name.charAt(0).toUpperCase(), 'participant-avatar');
      avatar.dataset['initial'] = p.name.charAt(0).toUpperCase();
      Object.assign(avatar.style, avatarColors(p.name, p.chatStyle?.color));
      const name = el(
        'button',
        undefined,
        `${classic ? 'classic-participant-name' : 'participant-name'} participant-name-button`,
      );
      name.type = 'button';
      const badge = getRoleBadgeSpan(p.role);
      if (badge) name.append(badge);
      name.append(document.createTextNode(p.name));
      if (local) name.append(el('span', ' (you)', 'you-tag'));
      participantHovercard.bind(name, p.id, p.name);
      row.append(avatar);
      if (classic) row.append(name);
      else {
        const info = el('div', undefined, 'participant-info');
        info.append(name);
        const media = el('div', undefined, 'participant-media-icons');
        for (const [enabled, label, svg] of [
          [hasAudio, 'Microphone', hasAudio ? icons.micOn() : icons.micOff()],
          [hasVideo, 'Camera', hasVideo ? icons.camOn() : icons.camOff()],
        ] as const) {
          const icon = el('span', undefined, `media-icon ${enabled ? 'active' : 'muted'}`);
          icon.title = `${label} ${enabled ? 'on' : 'off'}`;
          icon.setAttribute('role', 'img');
          icon.setAttribute('aria-label', icon.title);
          icon.insertAdjacentHTML('afterbegin', svg);
          media.append(icon);
        }
        row.append(info, media);
      }
      if (!local) row.append(participantActionButton(p.id, p.name));
    }
    retained.add(row);
    if (row === position) position = position.nextSibling;
    else list.insertBefore(row, position);
    community.decorateAvatar(
      row.querySelector<HTMLElement>('.participant-avatar')!,
      p.id,
      p.authenticated === true,
    );
  }
  for (const node of Array.from(list.children))
    if (!retained.has(node as HTMLElement)) node.remove();
  participantHovercard.refresh();
  if (focusedId && document.activeElement === document.body) {
    Array.from(retained)
      .find((node) => node.dataset['participantId'] === focusedId)
      ?.querySelector<HTMLButtonElement>('button')
      ?.focus();
  }
}

function participantActionButton(participantId: string, name: string): HTMLButtonElement {
  const button = document.createElement('button');
  button.type = 'button';
  button.className = 'participant-actions-button';
  button.textContent = '⋯';
  button.title = `Actions for ${name}`;
  button.setAttribute('aria-label', `Actions for ${name}`);
  button.addEventListener('click', (event) => {
    event.stopPropagation();
    const bounds = button.getBoundingClientRect();
    showModerationMenu(participantId, name, bounds.left, bounds.bottom);
  });
  return button;
}

function sortParticipantRoster(participants: Participant[]): void {
  const ranks: Record<string, number> = {
    owner: 5,
    admin: 4,
    moderator: 3,
    member: 2,
    user: 1,
    guest: 0,
  };
  participants.sort(
    (left, right) =>
      (ranks[right.role] ?? 0) - (ranks[left.role] ?? 0) || left.name.localeCompare(right.name),
  );
}

function renderRemoteTrack(
  participantId: string,
  participantName: string,
  track: MediaStreamTrack,
  _kind: 'audio' | 'video',
  source?: string,
): void {
  const isScreen = source === 'screen' || source === 'screen-audio';
  const tileKey = isScreen ? `${participantId}:screen` : participantId;
  let tile = remoteTiles.get(tileKey);

  if (!tile) {
    tile = document.createElement('div');
    tile.className = isScreen ? 'video-tile screen-share' : 'video-tile';
    tile.dataset['participantId'] = participantId;

    // Context menu for moderation on remote tiles
    tile.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      showModerationMenu(participantId, participantName, e.clientX, e.clientY);
    });

    // No-video avatar (not shown for screen share tiles)
    if (!isScreen) {
      const noVideoAvatar = document.createElement('div');
      noVideoAvatar.className = 'no-video-avatar';
      const initial = document.createElement('div');
      initial.className = 'avatar-initial';
      initial.textContent = participantName.charAt(0).toUpperCase();
      noVideoAvatar.appendChild(initial);
      tile.appendChild(noVideoAvatar);
    }

    const nameTag = document.createElement('div');
    nameTag.className = 'name-tag';
    nameTag.textContent = isScreen ? `${participantName} (Screen)` : participantName;
    tile.appendChild(nameTag);
    paintTile(tile, participantName, participantColor(participantId));
    const pin = document.createElement('button');
    pin.type = 'button';
    pin.className = 'tile-pin';
    pin.textContent = 'Pin';
    pin.setAttribute('aria-pressed', 'false');
    pin.setAttribute(
      'aria-label',
      `Pin ${isScreen ? `${participantName}'s screen` : participantName}`,
    );
    pin.addEventListener('click', () => setPinnedTile(pinnedTileKey === tileKey ? null : tileKey));
    tile.appendChild(pin);

    remoteTiles.set(tileKey, tile);
    videoGrid.appendChild(tile);
    updateVideoGridCount();
  }

  if (track.kind === 'video') {
    let video = tile.querySelector('video');
    if (!video) {
      video = document.createElement('video');
      video.autoplay = true;
      video.playsInline = true;
      tile.insertBefore(video, tile.firstChild);
    }
    observeFirstVideoFrame(video, telemetry.record);
    video.srcObject = new MediaStream([track]);
    if (!isScreen) observeTileSize(tileKey, participantId, video);
    const avatar = tile.querySelector('.no-video-avatar') as HTMLElement | null;
    if (avatar) avatar.style.display = 'none';
  } else {
    let audio = tile.querySelector('audio');
    if (!audio) {
      audio = document.createElement('audio');
      audio.autoplay = true;
      tile.appendChild(audio);
    }
    audio.srcObject = new MediaStream([track]);
  }
  mediaControls.attachTile(tile, participantId, participantName);
}

function removeRemoteTrack(
  participantId: string,
  _producerId: string,
  kind: 'audio' | 'video',
  source?: string,
): void {
  const isScreen = source === 'screen' || source === 'screen-audio';
  const tileKey = isScreen ? `${participantId}:screen` : participantId;
  const tile = remoteTiles.get(tileKey);
  if (!tile) return;

  if (kind === 'video') {
    stopObservingTileSize(tileKey);
    const video = tile.querySelector('video');
    if (video) {
      video.srcObject = null;
      video.remove();
    }
    const avatar = tile.querySelector('.no-video-avatar') as HTMLElement | null;
    if (avatar) avatar.style.display = '';
  } else {
    const audio = tile.querySelector('audio');
    if (audio) {
      audio.srcObject = null;
      audio.remove();
    }
  }

  // Remove tile entirely if no active media remains
  if (!tile.querySelector('video') && !tile.querySelector('audio')) {
    stopObservingTileSize(tileKey);
    mediaControls.detachTile(tile);
    tile.remove();
    remoteTiles.delete(tileKey);
    updateVideoGridCount();
  }
}

function handleParticipantLeft(participantId: string, participantName?: string): void {
  mediaControls.detachParticipant(participantId);
  lobbyWaiters.delete(participantId);
  const tile = remoteTiles.get(participantId);
  if (tile) {
    stopObservingTileSize(participantId);
    tile.remove();
    remoteTiles.delete(participantId);
  }
  // Also clean up screen share tile if present
  const screenTile = remoteTiles.get(`${participantId}:screen`);
  if (screenTile) {
    screenTile.remove();
    remoteTiles.delete(`${participantId}:screen`);
  }
  updateVideoGridCount();
  if (participantName) appendSystemMessage(`${participantName} left`);
}

function handleParticipantJoined(participantId: string, participantName: string): void {
  lobbyWaiters.delete(participantId);
  renderLobbyPanel();
  appendSystemMessage(`${participantName} joined`);
}

function appendSystemMessage(text: string): void {
  socialChat.system(text);
}

function renderConnectionQuality(quality: ConnectionQuality): void {
  qualityIndicator.className = `quality-dot quality-${quality}`;
  const labels: Record<ConnectionQuality, string> = {
    good: 'Good connection',
    fair: 'Fair connection',
    poor: 'Poor connection',
    unknown: 'Checking connection...',
  };
  qualityIndicator.title = labels[quality];
}

// --- Keyboard Shortcuts ---
function shortcutsBlocked(event: KeyboardEvent): boolean {
  if (
    event.defaultPrevented ||
    event.repeat ||
    event.ctrlKey ||
    event.metaKey ||
    event.altKey ||
    event.shiftKey
  )
    return true;
  if (!room || roomScreen.hidden || roomRecovering) return true;
  if (document.querySelector('dialog[open], .modal-overlay:not([hidden]), #mod-menu')) return true;
  return (
    event.target instanceof Element &&
    Boolean(
      event.target.closest(
        'input, textarea, select, button, a, summary, [contenteditable]:not([contenteditable="false"]), [role="textbox"], [role="separator"], [role="dialog"]',
      ),
    )
  );
}

document.addEventListener('keydown', (e) => {
  const key = e.key.toLowerCase();
  if (key === 'escape') {
    // Native dialogs handle Escape themselves, including preview cleanup and focus restoration.
    if (document.querySelector('dialog[open], [popover]:popover-open')) return;
    dismissAuth();
    dismissCreateRoom();
    document.getElementById('mod-menu')?.remove();
    return;
  }
  if (shortcutsBlocked(e)) return;

  // PTT keys: Space and T (only in PTT mode)
  if (micMode === 'ptt' && (key === ' ' || key === 't') && !e.repeat) {
    e.preventDefault();
    if (canStartBroadcast('microphone'))
      observeUiTask(pttActivate(), 'Could not enable push-to-talk');
    return;
  }

  switch (key) {
    case 'm':
      observeUiTask(toggleMicrophone(), 'Could not update the microphone');
      break;
    case 'v':
      observeUiTask(toggleCamera(), 'Could not update the camera');
      break;
    case 's':
      observeUiTask(toggleScreenShare(), 'Could not update screen sharing');
      break;
  }
});

document.addEventListener('keyup', (e) => {
  const key = e.key.toLowerCase();
  if (micMode === 'ptt' && (key === ' ' || key === 't')) {
    pttDeactivate();
  }
});

// Release PTT if window loses focus while key is held
window.addEventListener('blur', () => {
  if (pttHeld) pttDeactivate();
});

const diagnosticsButton = button(
  'Diagnostics',
  () => {
    const view = modal('Diagnostic summary');
    view.body.append(
      el(
        'p',
        'Review this summary before copying it to support. Its reference identifies this local report, not a server session. It contains app events, browser family and release information; it excludes names, messages, credentials and network addresses.',
      ),
    );
    const sharing = el('input');
    sharing.type = 'checkbox';
    sharing.checked = telemetry.sharingEnabled;
    sharing.addEventListener('change', () => telemetry.setSharing(sharing.checked));
    const sharingLabel = el('label', ' Share anonymous reliability measurements');
    sharingLabel.prepend(sharing);
    view.body.append(
      sharingLabel,
      el(
        'p',
        'Optional. Sends fixed event categories and timings to this server without account, room or support identifiers. Media measurements run only while enabled and the page is visible. Turning this off discards queued uploads.',
      ),
    );
    const preview = el('textarea');
    preview.readOnly = true;
    preview.rows = 16;
    preview.value = telemetry.summary();
    preview.setAttribute('aria-label', 'Diagnostic summary preview');
    const copy = button('Copy summary', () => {
      navigator.clipboard
        .writeText(preview.value)
        .then(() => showToast('Diagnostic summary copied'))
        .catch(() => {
          preview.focus();
          preview.select();
        });
    });
    const refresh = button('Refresh preview', () => {
      preview.value = telemetry.summary();
    });
    view.body.append(preview, refresh, copy);
  },
  'auth-link-btn',
);
diagnosticsButton.id = 'diagnostics-btn';
placeDiagnosticsButton(false);
