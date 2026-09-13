import './style.css';
import { SignalingClient } from './signaling';
import {
  RoomClient,
  RoomPasswordRequiredError,
  type Participant,
  type ConnectionQuality,
} from './room';
import * as icons from './icons';
import { AuthManager } from './auth';
import { MediaControls } from './media-controls';
import { SocialChat } from './social-chat';
import { CommunityUI } from './community-ui';
import { api, ApiError, safeRasterUrl } from './ui';
import { configureSettingsDialog } from './settings-dialog';
import { avatarColors } from './avatar-colors';
import './community.css';
import type { CreateRoomRequest } from './protocol';

// --- DOM refs ---
const connectionStatus = document.getElementById('connection-status')!;
const joinScreen = document.getElementById('join-screen')!;
const roomScreen = document.getElementById('room-screen')!;
const roomLabel = document.getElementById('room-label')!;
const roomTopic = document.getElementById('room-topic')!;
const nameInput = document.getElementById('name-input') as HTMLInputElement;
const roomInput = document.getElementById('room-input') as HTMLInputElement;
const joinBtn = document.getElementById('join-btn') as HTMLButtonElement;
const videoGrid = document.getElementById('video-grid')!;
const participantList = document.getElementById('participant-list')!;
const chatMessages = document.getElementById('chat-messages')!;
const chatInput = document.getElementById('chat-input') as HTMLInputElement;
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
const joinFormDivider = document.getElementById('join-form-divider')!;

// Login modal
const loginModal = document.getElementById('login-modal')!;
const loginClose = document.getElementById('login-close')!;
const loginEmail = document.getElementById('login-email') as HTMLInputElement;
const loginPassword = document.getElementById('login-password') as HTMLInputElement;
const loginSubmit = document.getElementById('login-submit') as HTMLButtonElement;
const loginError = document.getElementById('login-error')!;
const loginPasskeyBtn = document.getElementById('login-passkey-btn')!;
const loginToRegister = document.getElementById('login-to-register')!;

// Register modal
const registerModal = document.getElementById('register-modal')!;
const registerClose = document.getElementById('register-close')!;
const registerEmail = document.getElementById('register-email') as HTMLInputElement;
const registerName = document.getElementById('register-name') as HTMLInputElement;
const registerPassword = document.getElementById('register-password') as HTMLInputElement;
const registerConfirm = document.getElementById('register-confirm') as HTMLInputElement;
const registerSubmit = document.getElementById('register-submit') as HTMLButtonElement;
const registerError = document.getElementById('register-error')!;
const registerPasskeyBtn = document.getElementById('register-passkey-btn')!;
const registerToLogin = document.getElementById('register-to-login')!;

// Create room modal
const createRoomModal = document.getElementById('create-room-modal')!;
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

// Toast container
const toastContainer = document.getElementById('toast-container')!;

// Sidebar tabs
const sidebarTabs = document.querySelectorAll<HTMLButtonElement>('#sidebar-tabs .tab');
const tabContents = document.querySelectorAll<HTMLDivElement>('#sidebar-content .tab-content');

// Personal controls retain their values/listeners as they move into the shared dialog.
const layoutSelect = document.getElementById('layout-select') as HTMLSelectElement;
const micModeSelect = document.getElementById('mic-mode-select') as HTMLSelectElement;
const appearanceControls = document.getElementById('appearance-controls')!;
const microphoneControls = document.getElementById('microphone-controls')!;
document.getElementById('personal-settings-controls')!.remove();

// --- Auth ---
const auth = new AuthManager();

function updateAuthUI(): void {
  roomBrowser.hidden = false;
  joinFormDivider.hidden = false;
  createRoomBtn.hidden = !auth.isLoggedIn;
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
  observeUiTask(loadRoomBrowser(), 'Could not refresh the room directory');
  updateJoinBtn();
  community.refresh();
}

auth.setOnChange((loggedIn, tokenRefresh) => {
  updateAuthUI();
  if (loggedIn && tokenRefresh) {
    // Keep the current room session alive; the server will close the old
    // socket at JWT expiry and automatic reconnect will use this new token.
    signaling.setToken(auth.jwt ?? undefined);
    return;
  }
  // Identity changes leave the old membership before reconnecting.
  if (room) observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
  signaling.disconnect();
  signaling.connect(loggedIn ? (auth.jwt ?? undefined) : undefined);
});

// --- State ---
let room: RoomClient | null = null;
const mediaControls = new MediaControls({
  getRoom: () => room,
  notify: (message) => showToast(message),
  appearanceControls,
  microphoneControls,
});
mediaControls.mountToolbar(roomTools);
let localTextMuted = false;
let roomRecovering = false;
let cameraTogglePending = false;
let microphoneTogglePending = false;
const remoteTiles = new Map<string, HTMLDivElement>();
const lobbyWaiters = new Map<string, string>(); // participantId → displayName

// Active speaker / audio level tracking — avoids querySelectorAll on every event
const AUDIO_LEVEL_THRESHOLD = -50; // dB; only highlight above this
let currentDominantTile: HTMLDivElement | null = null;
const currentlySpeaking = new Set<HTMLElement>(); // tiles + list items with .speaking

// Push-to-Talk state
type MicMode = 'open' | 'ptt';
let personalMicMode: MicMode = localStorage.getItem('micMode') === 'ptt' ? 'ptt' : 'open';
let micMode: MicMode = personalMicMode;
let pttHeld = false;
let pttActivation = 0;

// Restore display name from localStorage
const savedName = localStorage.getItem('displayName');
if (savedName) nameInput.value = savedName;

// Check for room in URL hash
const hashRoom = window.location.hash.slice(1);
if (hashRoom) roomInput.value = hashRoom;

// --- Utility ---
function clearChildren(el: HTMLElement): void {
  while (el.firstChild) el.removeChild(el.firstChild);
}

// --- Toast Notifications ---
function showToast(message: string, duration = 3000): void {
  const toast = document.createElement('div');
  toast.className = 'toast';
  toast.textContent = message;
  toastContainer.appendChild(toast);
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
    if (room === expectedRoom && auth.userId === expectedUser) showToast(failureMessage);
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
): void {
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
  setTimeout(() => toast.remove(), duration);
}

// --- Moderation Context Menu ---
function showModerationMenu(targetId: string, targetName: string, x: number, y: number): void {
  document.getElementById('mod-menu')?.remove();

  const role = room?.role ?? 'user';
  const isMod = role === 'owner' || role === 'admin' || role === 'moderator';
  const isAdmin = role === 'owner' || role === 'admin';

  const items: { label: string; action: () => void; danger?: boolean }[] = [];

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
    items.push({ label: 'Close Camera', action: () => room?.closeCam(targetId) });
    items.push({ label: 'Cam Unban', action: () => room?.camUnban(targetId) });
    items.push({ label: 'Mute Text', action: () => room?.textMute(targetId) });
    items.push({ label: 'Text Unmute', action: () => room?.textUnmute(targetId) });
    items.push({ label: 'Kick', action: () => room?.kick(targetId), danger: true });
  }

  // Admin+ actions
  if (isAdmin && !isSelf) {
    items.push({ label: 'Cam Ban', action: () => room?.camBan(targetId), danger: true });
    items.push({ label: 'Ban…', action: () => community.ban(targetId, targetName), danger: true });
  }

  const menu = document.createElement('div');
  menu.id = 'mod-menu';
  menu.className = 'mod-context-menu';

  for (const item of items) {
    const btn = document.createElement('button');
    btn.textContent = item.label;
    if (item.danger) btn.className = 'danger';
    btn.addEventListener('click', () => {
      item.action();
      menu.remove();
    });
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
      btn.addEventListener('click', () => {
        room?.setRole(targetId, opt.value);
        menu.remove();
      });
      group.appendChild(btn);
    }
    menu.appendChild(group);
  }

  menu.style.left = `${Math.min(x, window.innerWidth - 180)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - (items.length + roleOptions.length + 2) * 36 - 16)}px`;
  menu.style.top = `${Math.max(8, parseInt(menu.style.top, 10))}px`;
  menu.style.maxHeight = `${window.innerHeight - 16}px`;
  menu.style.overflowY = 'auto';
  document.body.appendChild(menu);

  const close = (e: MouseEvent) => {
    if (!menu.contains(e.target as Node)) {
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

function getRoleBadgeSpan(role: string): HTMLSpanElement | null {
  const symbol = ROLE_SYMBOLS[role];
  if (!symbol) return null;
  const span = document.createElement('span');
  span.className = `role-badge role-${role}`;
  span.textContent = symbol;
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
    admitBtn.addEventListener('click', () => {
      room?.admitFromLobby(id);
      lobbyWaiters.delete(id);
      renderLobbyPanel();
    });

    const denyBtn = document.createElement('button');
    denyBtn.className = 'lobby-deny-btn';
    denyBtn.textContent = 'Deny';
    denyBtn.addEventListener('click', () => {
      room?.denyFromLobby(id);
      lobbyWaiters.delete(id);
      renderLobbyPanel();
    });

    actions.appendChild(admitBtn);
    actions.appendChild(denyBtn);
    li.appendChild(nameSpan);
    li.appendChild(actions);
    lobbyListEl.appendChild(li);
  }
}

function populateRoomSettingsModal(): void {
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
}

// --- Layout Management ---
function getLayout(): 'modern' | 'classic' {
  return localStorage.getItem('layout') === 'modern' ? 'modern' : 'classic';
}

function setLayout(layout: 'modern' | 'classic'): void {
  localStorage.setItem('layout', layout);
  roomScreen.classList.remove('layout-modern', 'layout-classic');
  roomScreen.classList.add(`layout-${layout}`);
  layoutSelect.value = layout;

  // In classic mode, hide the Users tab from the sidebar (users are in the left panel)
  // and force the Chat tab active
  if (usersTab) usersTab.hidden = layout === 'classic' && window.innerWidth > 768;
  if (usersTab?.hidden && usersTab.classList.contains('active')) selectSidebarTab('chat');
  applyPanelPreferences();

  // Re-render participants for classic mode
  if (room) renderParticipants(room.getParticipants());
}

// Initialize layout
layoutSelect.value = getLayout();

// --- Sidebar Tabs ---
sidebarTabs.forEach((tab) => {
  tab.addEventListener('click', () => {
    const target = tab.dataset['tab'];
    sidebarTabs.forEach((t) => t.classList.toggle('active', t === tab));
    tabContents.forEach((c) => c.classList.toggle('active', c.id === `${target}-panel`));
  });
});

// Wire lobby tab — queried at load time so hidden tabs may not be in sidebarTabs NodeList
lobbyTab.addEventListener('click', () => {
  document
    .querySelectorAll<HTMLButtonElement>('#sidebar-tabs .tab')
    .forEach((t) => t.classList.toggle('active', t === lobbyTab));
  document
    .querySelectorAll<HTMLDivElement>('#sidebar-content .tab-content')
    .forEach((c) => c.classList.toggle('active', c.id === 'lobby-panel'));
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
  usersTab.append(' Users');
}

interface PanelPreferences {
  rosterWidth: number;
  chatWidth: number;
  rosterCollapsed: boolean;
  chatCollapsed: boolean;
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
    };
  } catch {
    return { rosterWidth: 220, chatWidth: 320, rosterCollapsed: false, chatCollapsed: false };
  }
})();

function selectSidebarTab(tab: 'chat' | 'users' | 'lobby'): void {
  sidebarTabs.forEach((button) => button.classList.toggle('active', button.dataset['tab'] === tab));
  tabContents.forEach((content) =>
    content.classList.toggle('active', content.id === `${tab}-panel`),
  );
}

function applyPanelPreferences(): void {
  const desktop = window.innerWidth > 768;
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
  rosterToggleBtn.textContent = rosterCollapsed ? 'Show people' : 'People';
  chatToggleBtn.textContent = chatCollapsed ? 'Show chat' : 'Chat';
  rosterToggleBtn.setAttribute('aria-expanded', String(!rosterCollapsed));
  chatToggleBtn.setAttribute('aria-expanded', String(!chatCollapsed));
  const roster = document.getElementById('classic-users-panel');
  if (roster) roster.inert = rosterCollapsed;
  document.getElementById('sidebar')!.inert = chatCollapsed;
  if (usersTab) usersTab.hidden = desktop && getLayout() === 'classic';
  if (usersTab?.hidden && usersTab.classList.contains('active')) selectSidebarTab('chat');
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
    if (window.innerWidth <= 768 || event.button !== 0) return;
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
  if (getLayout() === 'classic' && window.innerWidth > 768) {
    panelPreferences.rosterCollapsed = !panelPreferences.rosterCollapsed;
  } else {
    panelPreferences.chatCollapsed = false;
    selectSidebarTab('users');
  }
  savePanelPreferences();
});
chatToggleBtn.addEventListener('click', () => {
  if (window.innerWidth > 768) panelPreferences.chatCollapsed = !panelPreferences.chatCollapsed;
  selectSidebarTab('chat');
  savePanelPreferences();
});
attachPanelResize(document.getElementById('sidebar')!, 'chat');
window.addEventListener('resize', applyPanelPreferences);
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
    try {
      await navigator.clipboard.writeText(url.toString());
      if (room === activeRoom) showToast('Room link copied');
    } catch {
      if (room === activeRoom) prompt('Copy this room link:', url.toString());
    }
  }, 'Could not copy the room link'),
);

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

// --- Scroll-to-bottom for chat ---
scrollBottomBtn.textContent = '';
scrollBottomBtn.insertAdjacentHTML('afterbegin', icons.scrollDown());
scrollBottomBtn.appendChild(unreadBadge);

// --- Signaling setup ---
const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
const wsUrl = `${wsProtocol}//${window.location.host}/ws`;
const signaling = new SignalingClient(wsUrl);
const socialChat = new SocialChat({
  getRoom: () => room,
  getViewerKey: () => auth.userId ?? 'guest',
  notify: (message) => showToast(message),
  participantAction: showModerationMenu,
});
const community = new CommunityUI({
  auth,
  getRoom: () => room,
  notify: (message) => showToast(message),
  onProfileChanged: (profile) => {
    auth.updateDisplayName(profile.display_name);
    if (room) renderParticipants(room.getParticipants());
  },
  onRoomsChanged: () => observeUiTask(loadRoomBrowser(), 'Could not refresh the room directory'),
  onRoomDeleted: async (id) => {
    if (room?.currentRoomId === id) await leaveCurrentRoom();
  },
  onSignedOut: async () => {
    await leaveCurrentRoom();
    auth.forgetSession();
  },
});

signaling.setOnStatusChange((status) => {
  connectionStatus.textContent = status.charAt(0).toUpperCase() + status.slice(1);
  connectionStatus.className = `status ${status}`;
  joinBtn.disabled = status !== 'connected' || !nameInput.value.trim() || !roomInput.value.trim();
});

// Try to restore auth session from cookie, then connect WS
observeUiTask(
  auth.tryRestore().then(() => {
    updateAuthUI();
    signaling.connect(auth.jwt ?? undefined);
  }),
  'Could not restore sign-in. Reload the page to retry.',
);

// --- Join form ---
function updateJoinBtn(): void {
  joinBtn.disabled = !signaling.connected || !nameInput.value.trim() || !roomInput.value.trim();
}

nameInput.addEventListener('input', updateJoinBtn);
roomInput.addEventListener('input', updateJoinBtn);

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
  const request = ++roomBrowserRequest;
  if (!append) {
    roomBrowserPage = 1;
    clearChildren(roomList);
    roomLoadMore.hidden = true;
  }
  roomLoadMore.disabled = true;
  try {
    const params = new URLSearchParams({ page: String(roomBrowserPage), limit: '20' });
    if (roomBrowserQuery) params.set('q', roomBrowserQuery);
    const rooms = await api.rooms(auth.jwt, params).catch((error: unknown) => {
      if (!(error instanceof ApiError)) throw error;
      const explanation =
        error.status === 404 || error.status === 503
          ? 'The room directory is unavailable on this server. Local guest rooms can still be joined by name below; browsing saved rooms requires a configured database.'
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
      card.setAttribute('aria-label', `Join ${r.display_name}`);
      card.addEventListener('keydown', (event) => {
        if (event.key === 'Enter' || event.key === ' ') {
          event.preventDefault();
          card.click();
        }
      });
      card.addEventListener('click', () => {
        roomInput.value = r.id;
        if (auth.displayName && !nameInput.value.trim()) {
          nameInput.value = auth.displayName;
        }
        updateJoinBtn();
        if (nameInput.value.trim()) joinBtn.focus();
        else nameInput.focus();
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
      count.textContent = `${r.participant_count} online · ${r.broadcaster_count ?? 0} broadcasting`;
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
signInBtn.addEventListener('click', () => {
  loginModal.hidden = false;
});
logoutBtn.addEventListener('click', () => {
  auth.logout().catch((error) => {
    showToast(error instanceof Error ? error.message : 'Sign out failed');
  });
});

// Login
loginClose.addEventListener('click', () => {
  loginModal.hidden = true;
});
loginModal.addEventListener('click', (e) => {
  if (e.target === loginModal) loginModal.hidden = true;
});

loginSubmit.addEventListener(
  'click',
  asyncUiAction(async () => {
    loginError.hidden = true;
    loginSubmit.disabled = true;
    loginSubmit.textContent = 'Signing in...';
    try {
      await auth.login(loginEmail.value.trim(), loginPassword.value);
      loginModal.hidden = true;
      loginEmail.value = '';
      loginPassword.value = '';
    } catch (e) {
      loginError.textContent = e instanceof Error ? e.message : 'Login failed';
      loginError.hidden = false;
    } finally {
      loginSubmit.disabled = false;
      loginSubmit.textContent = 'Sign In';
    }
  }, 'Could not complete sign-in'),
);

loginEmail.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') loginPassword.focus();
});
loginPassword.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') loginSubmit.click();
});

loginPasskeyBtn.addEventListener(
  'click',
  asyncUiAction(async () => {
    const email = loginEmail.value.trim();
    if (!email) {
      loginError.textContent = 'Enter your email first';
      loginError.hidden = false;
      return;
    }
    loginError.hidden = true;
    try {
      const options = await auth.passkeyLoginStart(email);
      const credential = await navigator.credentials.get(options);
      if (!credential) throw new Error('Passkey cancelled');
      await auth.passkeyLoginFinish(credential);
      loginModal.hidden = true;
      loginEmail.value = '';
      loginPassword.value = '';
    } catch (e) {
      loginError.textContent = e instanceof Error ? e.message : 'Passkey login failed';
      loginError.hidden = false;
    }
  }, 'Could not complete passkey sign-in'),
);

loginToRegister.addEventListener('click', () => {
  loginModal.hidden = true;
  registerModal.hidden = false;
});

// Register
registerClose.addEventListener('click', () => {
  registerModal.hidden = true;
});
registerModal.addEventListener('click', (e) => {
  if (e.target === registerModal) registerModal.hidden = true;
});

registerSubmit.addEventListener(
  'click',
  asyncUiAction(async () => {
    registerError.hidden = true;
    if (registerPassword.value !== registerConfirm.value) {
      registerError.textContent = 'Passwords do not match';
      registerError.hidden = false;
      return;
    }
    if (registerPassword.value.length < 8) {
      registerError.textContent = 'Password must be at least 8 characters';
      registerError.hidden = false;
      return;
    }
    registerSubmit.disabled = true;
    registerSubmit.textContent = 'Creating account...';
    try {
      await auth.register(
        registerEmail.value.trim(),
        registerName.value.trim(),
        registerPassword.value,
      );
      registerModal.hidden = true;
      registerEmail.value = '';
      registerName.value = '';
      registerPassword.value = '';
      registerConfirm.value = '';
    } catch (e) {
      registerError.textContent = e instanceof Error ? e.message : 'Registration failed';
      registerError.hidden = false;
    } finally {
      registerSubmit.disabled = false;
      registerSubmit.textContent = 'Create Account';
    }
  }, 'Could not complete registration'),
);

registerPasskeyBtn.addEventListener(
  'click',
  asyncUiAction(async () => {
    const email = registerEmail.value.trim();
    const displayName = registerName.value.trim();
    if (!email || !displayName) {
      registerError.textContent = 'Fill in email and display name first';
      registerError.hidden = false;
      return;
    }
    registerError.hidden = true;
    try {
      const options = await auth.passkeyRegisterStart(email, displayName);
      const credential = await navigator.credentials.create(options);
      if (!credential) throw new Error('Passkey registration cancelled');
      await auth.passkeyRegisterFinish(credential);
      registerModal.hidden = true;
      registerEmail.value = '';
      registerName.value = '';
      registerPassword.value = '';
      registerConfirm.value = '';
    } catch (e) {
      registerError.textContent = e instanceof Error ? e.message : 'Passkey registration failed';
      registerError.hidden = false;
    }
  }, 'Could not complete passkey registration'),
);

registerToLogin.addEventListener('click', () => {
  registerModal.hidden = true;
  loginModal.hidden = false;
});

// --- Create Room ---
createRoomBtn.addEventListener('click', () => {
  if (auth.isLoggedIn) createRoomModal.hidden = false;
});
createRoomClose.addEventListener('click', () => {
  createRoomModal.hidden = true;
});
createRoomModal.addEventListener('click', (e) => {
  if (e.target === createRoomModal) createRoomModal.hidden = true;
});

createRoomSubmit.addEventListener(
  'click',
  asyncUiAction(async () => {
    createRoomError.hidden = true;
    const id = crId.value.trim();
    const displayName = crName.value.trim();
    if (!id || !displayName) {
      createRoomError.textContent = 'Room ID and Display Name are required';
      createRoomError.hidden = false;
      return;
    }
    const roomPasswordBytes = new TextEncoder().encode(crPassword.value).length;
    if (crPassword.value && (roomPasswordBytes < 8 || roomPasswordBytes > 256)) {
      createRoomError.textContent = 'Room password must be 8-256 bytes';
      createRoomError.hidden = false;
      return;
    }
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

      createRoomModal.hidden = true;
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
    } catch (e) {
      createRoomError.textContent = e instanceof Error ? e.message : 'Failed to create room';
      createRoomError.hidden = false;
    } finally {
      createRoomSubmit.disabled = false;
      createRoomSubmit.textContent = 'Create Room';
    }
  }, 'Could not complete room creation'),
);

joinBtn.addEventListener(
  'click',
  asyncUiAction(async () => {
    const name = nameInput.value.trim();
    const roomId = roomInput.value.trim();
    if (!name || !roomId) return;

    localStorage.setItem('displayName', name);
    window.location.hash = roomId;

    joinBtn.disabled = true;
    joinBtn.textContent = 'Joining...';
    localTextMuted = false;
    roomRecovering = false;

    try {
      room = new RoomClient(signaling, {
        onBackgroundError: (message) => showToast(message),
        onParticipantsChanged: (participants) => {
          observeUiTask(socialChat.activate(), 'Could not refresh room conversations');
          renderParticipants(participants);
          socialChat.participantsChanged();
          community.refresh();
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
        onRemoteTrack: renderRemoteTrack,
        onRemoteTrackRemoved: removeRemoteTrack,
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
            const reasonText = reason ? `\nReason: ${reason}` : '';
            alert(`You have been ${action} from this room.${reasonText}`);
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
          socialChat.participantsChanged();
          updateRoomModeUI();
          applyRoomSettingsToUI();
          roomTopic.textContent = settings.topic ?? '';
          roomTopic.hidden = !settings.topic;
        },
        onTopicChanged: (topic, changedBy) => {
          roomTopic.textContent = topic;
          roomTopic.hidden = !topic;
          showToast(`Topic changed by ${changedBy}: ${topic}`);
        },
        onVoiceRequested: (participantId, displayName) => {
          const role = room?.role ?? 'user';
          const canGrant = role === 'owner' || role === 'admin' || role === 'moderator';
          if (canGrant) {
            showActionToast(`${displayName} is requesting voice`, [
              { label: 'Grant', action: () => room?.setRole(participantId, 2) },
              { label: 'Dismiss', action: () => {} },
            ]);
          } else {
            showToast(`${displayName} is requesting voice`);
          }
        },
        onLobbyWaiting: (roomName, topic, count) => {
          mediaControls.reset();
          roomTools.hidden = true;
          joinScreen.hidden = true;
          roomScreen.hidden = true;
          lobbyScreen.hidden = false;
          lobbyRoomName.textContent = roomName;
          lobbyTopic.textContent = topic ?? '';
          lobbyTopic.hidden = !topic;
          lobbyCount.textContent = `${count} participant${count !== 1 ? 's' : ''} in room`;
        },
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
          showToast('You have been admitted to the room');
        },
        onAdmissionComplete: () => {
          // Post-admission media + room state are ready — refresh buttons/settings UI
          applyJoinedRoomUI();
        },
        onLobbyDenied: (reason) => {
          observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
          alert(`Lobby access denied${reason ? `: ${reason}` : ''}`);
        },
        onRecoveryState: (state, message) => {
          roomRecovering = state !== 'connected';
          if (state === 'connected') {
            connectionStatus.textContent = 'Connected';
            connectionStatus.className = 'status connected';
            applyJoinedRoomUI();
            updateLocalTile();
            showToast(message ?? 'Room connection restored');
          } else {
            connectionStatus.textContent =
              state === 'reconnecting' ? 'Rejoining room…' : 'Room recovery failed';
            connectionStatus.className = `status ${state === 'reconnecting' ? 'connecting' : 'disconnected'}`;
            pttDeactivate();
            applyRoomSettingsToUI();
            if (state === 'failed')
              showActionToast(
                message ?? 'Unable to rejoin the room',
                [
                  {
                    label: 'Leave room',
                    action: () =>
                      observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room'),
                  },
                ],
                15000,
              );
          }
        },
        onPasswordRequired: async () => prompt('The room password is required to reconnect:'),
        onRoomClosed: (reason) => {
          observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
          showToast(reason, 8000);
        },
      });

      let status: 'joined' | 'lobby';
      try {
        status = await room.join(roomId, name);
      } catch (e) {
        // Password-protected room: ask once and retry
        if (e instanceof RoomPasswordRequiredError) {
          const pw = prompt('This room requires a password:');
          if (pw === null) {
            room = null;
            joinBtn.disabled = false;
            joinBtn.textContent = 'Join Room';
            updateJoinBtn();
            return;
          }
          status = await room.join(roomId, name, pw);
        } else {
          throw e;
        }
      }

      if (status === 'lobby') {
        // onLobbyWaiting already switched to the lobby screen. Room UI is applied
        // by onLobbyAdmitted/onAdmissionComplete if/when we are admitted.
        return;
      }

      joinScreen.hidden = true;
      roomScreen.hidden = false;
      applyJoinedRoomUI();
    } catch (e) {
      console.error('Failed to join:', e);
      alert(`Failed to join: ${e instanceof Error ? e.message : String(e)}`);
      room = null;
      joinBtn.disabled = false;
      joinBtn.textContent = 'Join Room';
    }
  }, 'Could not complete joining the room'),
);

/** Apply all in-room UI state (label, topic, control buttons, settings-driven UI).
 * Called on direct join, on lobby admission (media pending), and again once
 * post-admission media setup completes. Idempotent. */
function applyJoinedRoomUI(): void {
  if (!room) return;
  setLayout(getLayout());

  roomLabel.textContent = room.roomSettings?.displayName ?? room.currentRoomId ?? '';
  roomLabel.hidden = false;
  roomTools.hidden = false;

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
    room.onScreenShareStopped = () => {
      updateScreenButton(false);
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

// Topic click-to-edit for Admin+ — registered once at module level.
// (Registering inside the join handler stacked one listener per join.)
roomTopic.addEventListener('click', () => {
  const role = room?.role ?? 'user';
  if (role !== 'owner' && role !== 'admin') return;
  const current = room?.roomSettings?.topic ?? '';
  const newTopic = prompt('Enter new topic:', current);
  if (newTopic !== null && newTopic !== current) {
    room?.setTopic(newTopic);
  }
});

// --- Lobby Cancel ---
lobbyCancelBtn.addEventListener(
  'click',
  asyncUiAction(leaveCurrentRoom, 'Could not finish leaving the room'),
);

// --- Leave ---
async function leaveCurrentRoom(): Promise<void> {
  pttDeactivate();
  mediaControls.reset();
  const leavingRoom = room;
  room = null;
  socialChat.reset();
  community.refresh();
  await leavingRoom?.leave();
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
  lobbyWaiters.clear();

  // Remove classic users panel if present
  document.getElementById('classic-users-panel')?.remove();

  // Reset speaking/active-speaker tracking
  currentDominantTile = null;
  currentlySpeaking.clear();

  // Reset PTT state
  pttHeld = false;
  pttActivation++;

  // Reset mic/cam button states (clear inline opacity from no-media mode)
  micBtn.style.opacity = '';
  camBtn.style.opacity = '';
  micBtn.classList.remove('active', 'muted', 'ptt-active');
  camBtn.classList.remove('active', 'muted');
  screenBtn.classList.remove('active');
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
  roomTools.hidden = true;
  lobbyScreen.hidden = true;
  joinScreen.hidden = false;
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

leaveBtn.addEventListener(
  'click',
  asyncUiAction(leaveCurrentRoom, 'Could not finish leaving the room'),
);

// --- Control buttons ---
function updateMicButton(enabled: boolean): void {
  let tooltip: string;
  if (micMode === 'ptt') {
    tooltip = enabled ? 'Release to mute' : 'Hold Space/T to talk';
  } else {
    tooltip = enabled ? 'Mute (M)' : 'Unmute (M)';
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
    enabled ? 'Cam Off (V)' : 'Cam On (V)',
  );
  camBtn.classList.toggle('active', enabled);
  camBtn.classList.toggle('muted', !enabled);
}

function updateScreenButton(active: boolean): void {
  setButtonContent(
    screenBtn,
    active ? icons.screenShareOff() : icons.screenShare(),
    active ? 'Stop Sharing (S)' : 'Screen (S)',
  );
  screenBtn.classList.toggle('active', active);
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
  Object.assign(initial.style, avatarColors(name));
  initial.textContent = name.charAt(0).toUpperCase();
  noVideoAvatar.appendChild(initial);
  tile.insertBefore(noVideoAvatar, tile.firstChild);
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
        : 'Microphone stopped. Click Unmute (M) to restart.',
    );
  } else {
    showToast('Camera stopped. Click Cam On (V) to restart.');
  }
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
    showToast(error instanceof Error ? error.message : 'Could not enable microphone');
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
    showToast(error instanceof Error ? error.message : 'Could not enable microphone');
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
    if (!activeRoom.videoEnabled && !mediaControls.hasConfiguredSetup) {
      if (!(await mediaControls.openSetup('camera'))) return;
      if (
        room !== activeRoom ||
        membership !== activeRoom.membershipVersion ||
        !canStartBroadcast('camera')
      )
        return;
    }
    const enabled = await activeRoom.toggleVideo();
    if (room !== activeRoom || membership !== activeRoom.membershipVersion) return;
    updateCamButton(enabled);
    updateLocalTile();
  } catch (error) {
    if (room !== activeRoom || membership !== activeRoom.membershipVersion) return;
    updateCamButton(activeRoom.videoEnabled);
    showToast(error instanceof Error ? error.message : 'Could not enable camera');
  } finally {
    cameraTogglePending = false;
  }
}

async function toggleScreenShare(): Promise<void> {
  const activeRoom = room;
  if (!activeRoom?.hasMedia) return;
  const membership = activeRoom.membershipVersion;
  if (activeRoom.isScreenSharing) {
    activeRoom.stopScreenShare();
    updateScreenButton(false);
  } else if (canStartBroadcast('screen')) {
    const success = await activeRoom.startScreenShare();
    if (room === activeRoom && membership === activeRoom.membershipVersion)
      updateScreenButton(success);
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
handBtn.addEventListener('click', () => {
  if (!room) return;
  room.requestVoice();
  handBtn.classList.toggle('hand-raised');
  showToast(handBtn.classList.contains('hand-raised') ? 'Hand raised' : 'Hand lowered');
});

// --- Room Settings Modal ---
configureSettingsDialog(roomSettingsModal);
roomSettingsBtn.addEventListener('click', () => {
  populateRoomSettingsModal();
  roomSettingsModal.showModal();
});

// Room settings toggle handlers
const settingsToggles: [HTMLInputElement, string][] = [
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
  el.addEventListener('change', () => {
    room?.updateRoomSettings({ [key]: el.checked });
  });
}

rsTopic.addEventListener('change', () => {
  const topic = rsTopic.value.trim();
  room?.setTopic(topic);
});

rsPassword.addEventListener('change', () => {
  const password = rsPassword.value;
  const passwordBytes = new TextEncoder().encode(password).length;
  if (password && (passwordBytes < 8 || passwordBytes > 256)) {
    showToast('Room password must be 8-256 bytes');
    rsPassword.focus();
    return;
  }
  room?.updateRoomSettings({ password: password || null });
});

rsMaxBroadcasters.addEventListener('change', () => {
  const val = parseInt(rsMaxBroadcasters.value, 10);
  // null (not undefined) so "clear the limit" survives JSON serialization
  room?.updateRoomSettings({ maxBroadcasters: isNaN(val) ? null : val });
});

rsMaxParticipants.addEventListener('change', () => {
  const val = parseInt(rsMaxParticipants.value, 10);
  room?.updateRoomSettings({ maxParticipants: isNaN(val) ? null : val });
});

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
    localStorage.setItem('micMode', mode);
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
function updateVideoGridCount(): void {
  const count = videoGrid.children.length;
  if (count <= 1) videoGrid.dataset['count'] = '1';
  else if (count === 2) videoGrid.dataset['count'] = '2';
  else if (count <= 4) videoGrid.dataset['count'] = '4';
  else if (count <= 6) videoGrid.dataset['count'] = '6';
  else videoGrid.dataset['count'] = 'many';
}

// --- Rendering ---
function renderParticipants(participants: Map<string, Participant>): void {
  clearChildren(participantList);

  // Build a full list including the local user (server only sends remote participants)
  const allParticipants: Participant[] = [];
  if (room?.localParticipantId) {
    // Build local producers map from actual media state
    const localProducers = new Map<string, { kind: 'audio' | 'video'; source?: string }>();
    if (room.audioEnabled)
      localProducers.set('local-audio', { kind: 'audio', source: 'microphone' });
    if (room.videoEnabled) localProducers.set('local-video', { kind: 'video', source: 'camera' });
    allParticipants.push({
      id: room.localParticipantId,
      name: room.nickname || nameInput.value.trim(),
      role: room.role,
      producers: localProducers,
    });
  }
  for (const p of participants.values()) {
    allParticipants.push(p);
  }
  sortParticipantRoster(allParticipants);

  for (const p of allParticipants) {
    const li = document.createElement('li');
    li.dataset['participantId'] = p.id;

    // Context menu for moderation (only on remote participants)
    if (p.id !== room?.localParticipantId) {
      li.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        showModerationMenu(p.id, p.name, e.clientX, e.clientY);
      });
    }

    // Avatar
    const avatar = document.createElement('div');
    avatar.className = 'participant-avatar';
    Object.assign(avatar.style, avatarColors(p.name));
    avatar.textContent = p.name.charAt(0).toUpperCase();
    community.decorateAvatar(
      avatar,
      p.id,
      p.id === room?.localParticipantId ? auth.isLoggedIn : p.authenticated === true,
    );

    // Info
    const info = document.createElement('div');
    info.className = 'participant-info';
    const nameSpan = document.createElement('span');
    nameSpan.className = 'participant-name';

    // Role badge
    const badge = getRoleBadgeSpan(p.role);
    if (badge) nameSpan.appendChild(badge);

    nameSpan.appendChild(document.createTextNode(p.name));
    if (p.id === room?.localParticipantId) {
      const youTag = document.createElement('span');
      youTag.className = 'you-tag';
      youTag.textContent = '(you)';
      nameSpan.appendChild(youTag);
    }
    info.appendChild(nameSpan);

    // Media icons — built with DOM API using safe SVG from our icons module
    const mediaIcons = document.createElement('div');
    mediaIcons.className = 'participant-media-icons';

    const hasAudio = [...p.producers.values()].some((v) => v.kind === 'audio');
    const hasVideo = [...p.producers.values()].some((v) => v.kind === 'video');

    const micIcon = document.createElement('span');
    micIcon.className = `media-icon ${hasAudio ? 'active' : 'muted'}`;
    // These are static SVG strings from our own module, not user content
    micIcon.insertAdjacentHTML(
      'afterbegin',
      `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2">${
        hasAudio
          ? '<path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/>'
          : '<line x1="1" y1="1" x2="23" y2="23"/><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6"/>'
      }</svg>`,
    );
    mediaIcons.appendChild(micIcon);

    const camIcon = document.createElement('span');
    camIcon.className = `media-icon ${hasVideo ? 'active' : 'muted'}`;
    camIcon.insertAdjacentHTML(
      'afterbegin',
      `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2">${
        hasVideo
          ? '<polygon points="23 7 16 12 23 17 23 7"/><rect x="1" y="5" width="15" height="14" rx="2" ry="2"/>'
          : '<path d="M16 16v1a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h2"/><line x1="1" y1="1" x2="23" y2="23"/>'
      }</svg>`,
    );
    mediaIcons.appendChild(camIcon);

    li.appendChild(avatar);
    li.appendChild(info);
    li.appendChild(mediaIcons);
    if (p.id !== room?.localParticipantId) li.appendChild(participantActionButton(p.id, p.name));
    participantList.appendChild(li);
  }

  // Update classic users panel if in classic mode
  renderClassicUsersPanel(participants);
}

function renderClassicUsersPanel(participants: Map<string, Participant>): void {
  const layout = getLayout();
  let panel = document.getElementById('classic-users-panel');

  if (layout !== 'classic') {
    panel?.remove();
    return;
  }

  if (!panel) {
    panel = document.createElement('div');
    panel.id = 'classic-users-panel';

    const title = document.createElement('div');
    title.className = 'panel-title';
    title.textContent = 'Users';
    panel.appendChild(title);

    const list = document.createElement('ul');
    list.className = 'classic-user-list';
    panel.appendChild(list);

    roomScreen.insertBefore(panel, roomScreen.firstChild);
    attachPanelResize(panel, 'roster');
    applyPanelPreferences();
  }

  const list = panel.querySelector('.classic-user-list') as HTMLElement;
  clearChildren(list);

  // Include local user with actual media state
  const allParticipants: Participant[] = [];
  if (room?.localParticipantId) {
    const localProducers = new Map<string, { kind: 'audio' | 'video'; source?: string }>();
    if (room.audioEnabled)
      localProducers.set('local-audio', { kind: 'audio', source: 'microphone' });
    if (room.videoEnabled) localProducers.set('local-video', { kind: 'video', source: 'camera' });
    allParticipants.push({
      id: room.localParticipantId,
      name: room.nickname || nameInput.value.trim(),
      role: room.role,
      producers: localProducers,
    });
  }
  for (const p of participants.values()) {
    allParticipants.push(p);
  }
  sortParticipantRoster(allParticipants);
  panel.querySelector('.panel-title')!.textContent = `People (${allParticipants.length})`;

  for (const p of allParticipants) {
    const li = document.createElement('li');
    li.dataset['participantId'] = p.id;

    // Context menu for moderation (only on remote participants)
    if (p.id !== room?.localParticipantId) {
      li.addEventListener('contextmenu', (e) => {
        e.preventDefault();
        showModerationMenu(p.id, p.name, e.clientX, e.clientY);
      });
    }

    const avatar = document.createElement('div');
    avatar.className = 'participant-avatar';
    Object.assign(avatar.style, avatarColors(p.name));
    avatar.style.width = '24px';
    avatar.style.height = '24px';
    avatar.style.fontSize = '0.65rem';
    avatar.textContent = p.name.charAt(0).toUpperCase();
    community.decorateAvatar(
      avatar,
      p.id,
      p.id === room?.localParticipantId ? auth.isLoggedIn : p.authenticated === true,
    );

    const nameSpan = document.createElement('span');
    nameSpan.className = 'classic-participant-name';
    // Role badge in classic panel
    const classicBadge = getRoleBadgeSpan(p.role);
    if (classicBadge) nameSpan.appendChild(classicBadge);
    nameSpan.appendChild(document.createTextNode(p.name));
    if (p.id === room?.localParticipantId) {
      nameSpan.appendChild(document.createTextNode(' (you)'));
    }

    li.appendChild(avatar);
    li.appendChild(nameSpan);
    if (p.id !== room?.localParticipantId) li.appendChild(participantActionButton(p.id, p.name));
    list.appendChild(li);
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
      Object.assign(initial.style, avatarColors(participantName));
      initial.textContent = participantName.charAt(0).toUpperCase();
      noVideoAvatar.appendChild(initial);
      tile.appendChild(noVideoAvatar);
    }

    const nameTag = document.createElement('div');
    nameTag.className = 'name-tag';
    nameTag.textContent = isScreen ? `${participantName} (Screen)` : participantName;
    tile.appendChild(nameTag);

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
    video.srcObject = new MediaStream([track]);
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
    tile.remove();
    remoteTiles.delete(tileKey);
    updateVideoGridCount();
  }
}

function handleParticipantLeft(participantId: string): void {
  mediaControls.detachParticipant(participantId);
  lobbyWaiters.delete(participantId);
  const tile = remoteTiles.get(participantId);
  const name = tile?.querySelector('.name-tag')?.textContent;
  if (tile) {
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
  if (name) appendSystemMessage(`${name} left`);
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
        'input, textarea, select, button, a, summary, [contenteditable]:not([contenteditable="false"]), [role="textbox"], [role="separator"]',
      ),
    )
  );
}

document.addEventListener('keydown', (e) => {
  const key = e.key.toLowerCase();
  if (key === 'escape') {
    // Native dialogs handle Escape themselves, including preview cleanup and focus restoration.
    if (document.querySelector('dialog[open]')) return;
    loginModal.hidden = true;
    registerModal.hidden = true;
    createRoomModal.hidden = true;
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
