import './style.css';
import { SignalingClient } from './signaling';
import { RoomClient, type Participant, type ConnectionQuality } from './room';
import * as icons from './icons';

// --- DOM refs ---
const connectionStatus = document.getElementById('connection-status')!;
const joinScreen = document.getElementById('join-screen')!;
const roomScreen = document.getElementById('room-screen')!;
const roomLabel = document.getElementById('room-label')!;
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

// Lobby screen
const lobbyScreen = document.getElementById('lobby-screen')!;
const lobbyRoomName = document.getElementById('lobby-room-name')!;
const lobbyTopic = document.getElementById('lobby-topic')!;
const lobbyCount = document.getElementById('lobby-count')!;
const lobbyCancelBtn = document.getElementById('lobby-cancel-btn')!;

// Room settings modal
const roomSettingsModal = document.getElementById('room-settings-modal')!;
const roomSettingsClose = document.getElementById('room-settings-close')!;
const rsModerated = document.getElementById('rs-moderated') as HTMLInputElement;
const rsLobby = document.getElementById('rs-lobby') as HTMLInputElement;
const rsScreen = document.getElementById('rs-screen') as HTMLInputElement;
const rsChat = document.getElementById('rs-chat') as HTMLInputElement;
const rsGuests = document.getElementById('rs-guests') as HTMLInputElement;

// Toast container
const toastContainer = document.getElementById('toast-container')!;

// Sidebar tabs
const sidebarTabs = document.querySelectorAll<HTMLButtonElement>('#sidebar-tabs .tab');
const tabContents = document.querySelectorAll<HTMLDivElement>('#sidebar-content .tab-content');

// Settings modal
const settingsModal = document.getElementById('settings-modal')!;
const settingsClose = document.getElementById('settings-close')!;
const layoutSelect = document.getElementById('layout-select') as HTMLSelectElement;
const micModeSelect = document.getElementById('mic-mode-select') as HTMLSelectElement;
const cameraSelect = document.getElementById('camera-select') as HTMLSelectElement;
const micSelect = document.getElementById('mic-select') as HTMLSelectElement;

// --- State ---
let room: RoomClient | null = null;
const remoteTiles = new Map<string, HTMLDivElement>();
let unreadCount = 0;
let isAtBottom = true;

// Active speaker / audio level tracking — avoids querySelectorAll on every event
const AUDIO_LEVEL_THRESHOLD = -50; // dB; only highlight above this
let currentDominantTile: HTMLDivElement | null = null;
const currentlySpeaking = new Set<HTMLElement>(); // tiles + list items with .speaking

// Push-to-Talk state
type MicMode = 'open' | 'ptt';
let micMode: MicMode = (localStorage.getItem('micMode') as MicMode) || 'open';
let pttHeld = false;

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

/** Deterministic HSL color from a string (for sender names, avatars) */
function nameColor(name: string): string {
  let hash = 0;
  for (let i = 0; i < name.length; i++) {
    hash = name.charCodeAt(i) + ((hash << 5) - hash);
  }
  const hue = Math.abs(hash) % 360;
  return `hsl(${hue}, 65%, 55%)`;
}

/**
 * Auto-link URLs in text.
 * HTML-escapes all user content first to prevent XSS, then wraps URLs in anchor tags.
 * The only HTML produced is the <a> tags with escaped href values.
 */
function linkify(text: string): string {
  // First: escape ALL user content to prevent XSS
  const escaped = text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#x27;');
  // Then: wrap URLs (which are now safe escaped text) in anchor tags
  return escaped.replace(
    /(https?:\/\/[^\s<&]+)/g,
    '<a href="$1" target="_blank" rel="noopener noreferrer">$1</a>',
  );
}

/** Format time as HH:MM */
function formatTime(date: Date): string {
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
}

// --- Toast Notifications ---
function showToast(message: string, duration = 3000): void {
  const toast = document.createElement('div');
  toast.className = 'toast';
  toast.textContent = message;
  toastContainer.appendChild(toast);
  setTimeout(() => toast.remove(), duration);
}

// --- Moderation Context Menu ---
function showModerationMenu(targetId: string, _targetName: string, x: number, y: number): void {
  // Remove any existing menu
  document.getElementById('mod-menu')?.remove();

  const role = room?.role ?? 'user';

  const items: { label: string; action: () => void; danger?: boolean }[] = [];

  if (role === 'owner' || role === 'admin' || role === 'moderator') {
    items.push({ label: 'Close Camera', action: () => room?.closeCam(targetId) });
    items.push({ label: 'Mute Text', action: () => room?.textMute(targetId) });
    items.push({ label: 'Kick', action: () => room?.kick(targetId), danger: true });
  }
  if (role === 'owner' || role === 'admin') {
    items.push({ label: 'Cam Ban', action: () => room?.camBan(targetId), danger: true });
    items.push({ label: 'Ban', action: () => room?.ban(targetId), danger: true });
  }

  if (items.length === 0) return; // Don't show empty menu for regular users

  const menu = document.createElement('div');
  menu.id = 'mod-menu';
  menu.className = 'mod-context-menu';

  for (const item of items) {
    const btn = document.createElement('button');
    btn.textContent = item.label;
    if (item.danger) btn.className = 'danger';
    btn.addEventListener('click', () => { item.action(); menu.remove(); });
    menu.appendChild(btn);
  }

  // Position the menu, ensuring it stays within the viewport
  menu.style.left = `${Math.min(x, window.innerWidth - 180)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - items.length * 36 - 16)}px`;
  document.body.appendChild(menu);

  // Close on click outside
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
  const isPrivileged = role === 'owner' || role === 'admin' || role === 'moderator' || role === 'member';
  handBtn.hidden = !(settings?.moderated && !isPrivileged);

  // Show room settings button for admins+
  roomSettingsBtn.hidden = !(role === 'owner' || role === 'admin');
}

function populateRoomSettingsModal(): void {
  const settings = room?.roomSettings;
  if (!settings) return;
  rsModerated.checked = settings.moderated;
  rsLobby.checked = settings.lobbyEnabled;
  rsScreen.checked = settings.allowScreenSharing;
  rsChat.checked = settings.allowChat;
  rsGuests.checked = settings.guestsAllowed;
}

// --- Layout Management ---
function getLayout(): 'modern' | 'classic' {
  return (localStorage.getItem('layout') as 'modern' | 'classic') || 'modern';
}

function setLayout(layout: 'modern' | 'classic'): void {
  localStorage.setItem('layout', layout);
  roomScreen.className = `layout-${layout}`;
  layoutSelect.value = layout;

  // In classic mode, hide the Users tab from the sidebar (users are in the left panel)
  // and force the Chat tab active
  if (usersTab) usersTab.hidden = layout === 'classic';
  if (layout === 'classic') {
    sidebarTabs.forEach((t) => t.classList.toggle('active', t.dataset['tab'] === 'chat'));
    tabContents.forEach((c) => c.classList.toggle('active', c.id === 'chat-panel'));
  }

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
setButtonContent(settingsBtn, icons.settings(), 'Settings');
setButtonContent(leaveBtn, icons.leave(), 'Leave');

// --- Scroll-to-bottom for chat ---
scrollBottomBtn.textContent = '';
scrollBottomBtn.insertAdjacentHTML('afterbegin', icons.scrollDown());

chatMessages.addEventListener('scroll', () => {
  const threshold = 40;
  isAtBottom = chatMessages.scrollHeight - chatMessages.scrollTop - chatMessages.clientHeight < threshold;
  if (isAtBottom) {
    unreadCount = 0;
    scrollBottomBtn.hidden = true;
    unreadBadge.hidden = true;
  } else {
    scrollBottomBtn.hidden = false;
  }
});

scrollBottomBtn.addEventListener('click', () => {
  chatMessages.scrollTop = chatMessages.scrollHeight;
  unreadCount = 0;
  scrollBottomBtn.hidden = true;
  unreadBadge.hidden = true;
});

// --- Signaling setup ---
const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
const wsUrl = `${wsProtocol}//${window.location.host}/ws`;
const signaling = new SignalingClient(wsUrl);

signaling.setOnStatusChange((status) => {
  connectionStatus.textContent = status.charAt(0).toUpperCase() + status.slice(1);
  connectionStatus.className = `status ${status}`;
  joinBtn.disabled = status !== 'connected' || !nameInput.value.trim() || !roomInput.value.trim();
});

signaling.connect();

// --- Join form ---
function updateJoinBtn(): void {
  joinBtn.disabled = !signaling.connected || !nameInput.value.trim() || !roomInput.value.trim();
}

nameInput.addEventListener('input', updateJoinBtn);
roomInput.addEventListener('input', updateJoinBtn);

nameInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') joinBtn.click(); });
roomInput.addEventListener('keydown', (e) => { if (e.key === 'Enter') joinBtn.click(); });

joinBtn.addEventListener('click', async () => {
  const name = nameInput.value.trim();
  const roomId = roomInput.value.trim();
  if (!name || !roomId) return;

  localStorage.setItem('displayName', name);
  window.location.hash = roomId;

  joinBtn.disabled = true;
  joinBtn.textContent = 'Joining...';

  try {
    room = new RoomClient(signaling, {
      onParticipantsChanged: renderParticipants,
      onLocalStream: () => {}, // Unused — local tile managed by updateLocalTile on user action
      onRemoteTrack: renderRemoteTrack,
      onRemoteTrackRemoved: removeRemoteTrack,
      onParticipantLeft: handleParticipantLeft,
      onChatMessage: appendChatMessage,
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
            const listItem = participantList.querySelector<HTMLElement>(`[data-participant-id="${CSS.escape(participantId)}"]`);
            if (listItem) {
              listItem.classList.add('speaking');
              currentlySpeaking.add(listItem);
            }
            // Classic users panel
            const classicItem = document.getElementById('classic-users-panel')
              ?.querySelector<HTMLElement>(`[data-participant-id="${CSS.escape(participantId)}"]`);
            if (classicItem) {
              classicItem.classList.add('speaking');
              currentlySpeaking.add(classicItem);
            }
          }
        }
      },
      onModeration: (action, participantId, reason) => {
        // Find participant name
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
        const participants = room?.getParticipants();
        const targetName = participants?.get(participantId)?.name
          ?? (participantId === room?.localParticipantId ? nameInput.value.trim() : participantId.slice(0, 8));
        if (participantId === room?.localParticipantId) {
          showToast(`Your role has been changed to ${newRole}`);
        } else {
          showToast(`${targetName} is now ${newRole}`);
        }
        updateRoomModeUI();
        // Re-render participants to update role badges
        if (room) renderParticipants(room.getParticipants());
      },
      onRoomSettingsChanged: (_settings) => {
        showToast('Room settings have been updated');
        updateRoomModeUI();
      },
      onTopicChanged: (topic, changedBy) => {
        showToast(`Topic changed by ${changedBy}: ${topic}`);
      },
      onVoiceRequested: (_participantId, displayName) => {
        showToast(`${displayName} is requesting voice`);
      },
      onLobbyWaiting: (roomName, topic, count) => {
        joinScreen.hidden = true;
        roomScreen.hidden = true;
        lobbyScreen.hidden = false;
        lobbyRoomName.textContent = roomName;
        lobbyTopic.textContent = topic ?? '';
        lobbyTopic.hidden = !topic;
        lobbyCount.textContent = `${count} participant${count !== 1 ? 's' : ''} in room`;
      },
      onLobbyJoin: (_participantId, displayName) => {
        showToast(`${displayName} is waiting in the lobby`);
      },
      onLobbyAdmitted: () => {
        lobbyScreen.hidden = true;
        roomScreen.hidden = false;
        showToast('You have been admitted to the room');
      },
      onLobbyDenied: (reason) => {
        lobbyScreen.hidden = true;
        joinScreen.hidden = false;
        joinBtn.disabled = false;
        joinBtn.textContent = 'Join Room';
        alert(`Lobby access denied${reason ? `: ${reason}` : ''}`);
      },
    });

    await room.join(roomId, name);

    joinScreen.hidden = true;
    roomScreen.hidden = false;
    setLayout(getLayout());

    // Show room label in header
    roomLabel.textContent = roomId;
    roomLabel.hidden = false;

    // Update control buttons — always start muted + cam off (privacy)
    if (room.hasMedia) {
      updateMicButton(false);
      updateCamButton(false);
      updateScreenButton(false);
      // Register callback so UI updates when browser's "Stop sharing" button is clicked
      room.onScreenShareStopped = () => {
        updateScreenButton(false);
      };
    } else {
      // Media unavailable — show disabled state
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
  } catch (e) {
    console.error('Failed to join:', e);
    alert(`Failed to join: ${e instanceof Error ? e.message : String(e)}`);
    room = null;
    joinBtn.disabled = false;
    joinBtn.textContent = 'Join Room';
  }
});

// --- Lobby Cancel ---
lobbyCancelBtn.addEventListener('click', async () => {
  await room?.leave();
  room = null;
  lobbyScreen.hidden = true;
  joinScreen.hidden = false;
  joinBtn.disabled = false;
  joinBtn.textContent = 'Join Room';
  updateJoinBtn();
});

// --- Leave ---
leaveBtn.addEventListener('click', async () => {
  await room?.leave();
  room = null;

  clearChildren(videoGrid);
  clearChildren(participantList);
  clearChildren(chatMessages);
  remoteTiles.clear();

  // Remove classic users panel if present
  document.getElementById('classic-users-panel')?.remove();

  // Reset speaking/active-speaker tracking
  currentDominantTile = null;
  currentlySpeaking.clear();

  // Reset PTT state
  pttHeld = false;

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

  roomScreen.hidden = true;
  lobbyScreen.hidden = true;
  joinScreen.hidden = false;
  roomLabel.hidden = true;
  joinBtn.textContent = 'Join Room';
  qualityIndicator.hidden = true;
  qualityIndicator.className = 'quality-dot';
  updateJoinBtn();
});

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
  setButtonContent(camBtn, enabled ? icons.camOn() : icons.camOff(), enabled ? 'Cam Off (V)' : 'Cam On (V)');
  camBtn.classList.toggle('active', enabled);
  camBtn.classList.toggle('muted', !enabled);
}

function updateScreenButton(active: boolean): void {
  setButtonContent(screenBtn, active ? icons.screenShareOff() : icons.screenShare(), active ? 'Stop Sharing (S)' : 'Screen (S)');
  screenBtn.classList.toggle('active', active);
}

/** Show/hide/update the local video tile based on current mic+cam state */
function updateLocalTile(): void {
  if (!room) return;
  const camOn = room.videoEnabled;
  const micOn = room.audioEnabled;
  const tile = document.getElementById('local-tile');
  const localName = nameInput.value.trim();

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
  initial.style.background = nameColor(name);
  initial.textContent = name.charAt(0).toUpperCase();
  noVideoAvatar.appendChild(initial);
  tile.insertBefore(noVideoAvatar, tile.firstChild);
}

async function pttActivate(): Promise<void> {
  if (!room || !room.hasMedia || pttHeld) return;
  pttHeld = true;
  await room.unmuteAudio();
  updateMicButton(true);
  updateLocalTile();
}

function pttDeactivate(): void {
  if (!room || !pttHeld) return;
  pttHeld = false;
  room.muteAudio();
  updateMicButton(false);
  updateLocalTile();
}

micBtn.addEventListener('mousedown', (e) => {
  if (!room || !room.hasMedia) return;
  if (micMode === 'ptt') {
    e.preventDefault(); // prevent focus loss
    pttActivate();
  }
});

micBtn.addEventListener('mouseup', () => {
  if (micMode === 'ptt') pttDeactivate();
});

micBtn.addEventListener('mouseleave', () => {
  if (micMode === 'ptt') pttDeactivate();
});

micBtn.addEventListener('click', async () => {
  if (!room || !room.hasMedia) return;
  if (micMode === 'open') {
    const enabled = await room.toggleAudio();
    updateMicButton(enabled);
    updateLocalTile();
  }
  // In PTT mode, click is handled by mousedown/mouseup above
});

camBtn.addEventListener('click', async () => {
  if (!room) return;
  if (!room.hasMedia) return;
  const enabled = await room.toggleVideo();
  updateCamButton(enabled);
  updateLocalTile();
});

screenBtn.addEventListener('click', async () => {
  if (!room || !room.hasMedia) return;
  if (room.isScreenSharing) {
    room.stopScreenShare();
    updateScreenButton(false);
  } else {
    const success = await room.startScreenShare();
    updateScreenButton(success);
  }
});

// --- Hand raise ---
handBtn.addEventListener('click', () => {
  if (!room) return;
  room.requestVoice();
  handBtn.classList.toggle('hand-raised');
  showToast(handBtn.classList.contains('hand-raised') ? 'Hand raised' : 'Hand lowered');
});

// --- Room Settings Modal ---
roomSettingsBtn.addEventListener('click', () => {
  populateRoomSettingsModal();
  roomSettingsModal.hidden = false;
});

roomSettingsClose.addEventListener('click', () => {
  roomSettingsModal.hidden = true;
});

roomSettingsModal.addEventListener('click', (e) => {
  if (e.target === roomSettingsModal) roomSettingsModal.hidden = true;
});

rsModerated.addEventListener('change', () => {
  room?.updateRoomSettings({ moderated: rsModerated.checked });
});

rsLobby.addEventListener('change', () => {
  room?.updateRoomSettings({ lobbyEnabled: rsLobby.checked });
});

rsScreen.addEventListener('change', () => {
  room?.updateRoomSettings({ allowScreenSharing: rsScreen.checked });
});

rsChat.addEventListener('change', () => {
  room?.updateRoomSettings({ allowChat: rsChat.checked });
});

rsGuests.addEventListener('change', () => {
  room?.updateRoomSettings({ guestsAllowed: rsGuests.checked });
});

// --- Settings Modal ---
settingsBtn.addEventListener('click', () => {
  settingsModal.hidden = false;
  populateDeviceSelectors();
});

settingsClose.addEventListener('click', () => {
  settingsModal.hidden = true;
});

// Close on backdrop click
settingsModal.addEventListener('click', (e) => {
  if (e.target === settingsModal) settingsModal.hidden = true;
});

layoutSelect.addEventListener('change', () => {
  setLayout(layoutSelect.value as 'modern' | 'classic');
});

// Mic mode setting
micModeSelect.value = micMode;

function setMicMode(mode: MicMode): void {
  micMode = mode;
  localStorage.setItem('micMode', mode);
  micModeSelect.value = mode;

  if (room && room.hasMedia) {
    if (mode === 'ptt') {
      // Entering PTT mode — mute immediately
      pttHeld = false;
      room.muteAudio();
      updateMicButton(false);
      updateLocalTile();
    } else {
      // Entering open mic mode — reset PTT state, leave mic as-is (muted)
      pttHeld = false;
      updateMicButton(room.audioEnabled);
    }
  }
}

micModeSelect.addEventListener('change', () => {
  setMicMode(micModeSelect.value as MicMode);
});

cameraSelect.addEventListener('change', async () => {
  const deviceId = cameraSelect.value;
  if (deviceId && room) {
    try {
      await room.switchCamera(deviceId);
    } catch (e) {
      console.error('Failed to switch camera:', e);
    }
  }
});

micSelect.addEventListener('change', async () => {
  const deviceId = micSelect.value;
  if (deviceId && room) {
    try {
      await room.switchMic(deviceId);
    } catch (e) {
      console.error('Failed to switch mic:', e);
    }
  }
});

async function populateDeviceSelectors(): Promise<void> {
  try {
    const devices = await navigator.mediaDevices.enumerateDevices();

    // Camera
    while (cameraSelect.firstChild) cameraSelect.removeChild(cameraSelect.firstChild);
    const cameras = devices.filter((d) => d.kind === 'videoinput');
    cameras.forEach((d, i) => {
      const opt = document.createElement('option');
      opt.value = d.deviceId;
      opt.textContent = d.label || `Camera ${i + 1}`;
      cameraSelect.appendChild(opt);
    });

    // Mic
    while (micSelect.firstChild) micSelect.removeChild(micSelect.firstChild);
    const mics = devices.filter((d) => d.kind === 'audioinput');
    mics.forEach((d, i) => {
      const opt = document.createElement('option');
      opt.value = d.deviceId;
      opt.textContent = d.label || `Microphone ${i + 1}`;
      micSelect.appendChild(opt);
    });
  } catch (e) {
    console.warn('Could not enumerate devices:', e);
  }
}

// --- Chat ---
function sendChatMessage(): void {
  const content = chatInput.value.trim();
  if (!content || !room) return;
  room.sendChat(content);
  const name = nameInput.value.trim();
  appendChatMessage(room.localParticipantId ?? '', name, content);
  chatInput.value = '';
}

chatSendBtn.addEventListener('click', sendChatMessage);
chatInput.addEventListener('keydown', (e) => {
  if (e.key === 'Enter') sendChatMessage();
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
    if (room.audioEnabled) localProducers.set('local-audio', { kind: 'audio', source: 'microphone' });
    if (room.videoEnabled) localProducers.set('local-video', { kind: 'video', source: 'camera' });
    allParticipants.push({
      id: room.localParticipantId,
      name: nameInput.value.trim(),
      role: room.role,
      producers: localProducers,
    });
  }
  for (const p of participants.values()) {
    allParticipants.push(p);
  }

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
    avatar.style.background = nameColor(p.name);
    avatar.textContent = p.name.charAt(0).toUpperCase();

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
    micIcon.insertAdjacentHTML('afterbegin',
      `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2">${hasAudio
        ? '<path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z"/>'
        : '<line x1="1" y1="1" x2="23" y2="23"/><path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6"/>'
      }</svg>`);
    mediaIcons.appendChild(micIcon);

    const camIcon = document.createElement('span');
    camIcon.className = `media-icon ${hasVideo ? 'active' : 'muted'}`;
    camIcon.insertAdjacentHTML('afterbegin',
      `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2">${hasVideo
        ? '<polygon points="23 7 16 12 23 17 23 7"/><rect x="1" y="5" width="15" height="14" rx="2" ry="2"/>'
        : '<path d="M16 16v1a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h2"/><line x1="1" y1="1" x2="23" y2="23"/>'
      }</svg>`);
    mediaIcons.appendChild(camIcon);

    li.appendChild(avatar);
    li.appendChild(info);
    li.appendChild(mediaIcons);
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
  }

  const list = panel.querySelector('.classic-user-list') as HTMLElement;
  clearChildren(list);

  // Include local user with actual media state
  const allParticipants: Participant[] = [];
  if (room?.localParticipantId) {
    const localProducers = new Map<string, { kind: 'audio' | 'video'; source?: string }>();
    if (room.audioEnabled) localProducers.set('local-audio', { kind: 'audio', source: 'microphone' });
    if (room.videoEnabled) localProducers.set('local-video', { kind: 'video', source: 'camera' });
    allParticipants.push({
      id: room.localParticipantId,
      name: nameInput.value.trim(),
      role: room.role,
      producers: localProducers,
    });
  }
  for (const p of participants.values()) {
    allParticipants.push(p);
  }

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
    avatar.style.background = nameColor(p.name);
    avatar.style.width = '24px';
    avatar.style.height = '24px';
    avatar.style.fontSize = '0.65rem';
    avatar.textContent = p.name.charAt(0).toUpperCase();

    const nameSpan = document.createElement('span');
    // Role badge in classic panel
    const classicBadge = getRoleBadgeSpan(p.role);
    if (classicBadge) nameSpan.appendChild(classicBadge);
    nameSpan.appendChild(document.createTextNode(p.name));
    if (p.id === room?.localParticipantId) {
      nameSpan.appendChild(document.createTextNode(' (you)'));
    }

    li.appendChild(avatar);
    li.appendChild(nameSpan);
    list.appendChild(li);
  }
}


function renderRemoteTrack(participantId: string, participantName: string, track: MediaStreamTrack, _kind: 'audio' | 'video', source?: string): void {
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
      initial.style.background = nameColor(participantName);
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
}

function removeRemoteTrack(participantId: string, _producerId: string, kind: 'audio' | 'video', source?: string): void {
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
  void participantId; // unused but part of callback signature
  appendSystemMessage(`${participantName} joined`);
}

function appendChatMessage(participantId: string, participantName: string, content: string): void {
  const div = document.createElement('div');
  div.className = 'chat-msg';

  const isLocal = participantId === room?.localParticipantId;

  const sender = document.createElement('span');
  sender.className = 'sender';
  sender.textContent = isLocal ? 'You' : participantName;
  sender.style.color = isLocal ? 'var(--accent)' : nameColor(participantName);

  const msgText = document.createElement('div');
  msgText.className = 'msg-text';
  // linkify() escapes all HTML entities before wrapping URLs in anchor tags
  msgText.insertAdjacentHTML('afterbegin', linkify(content));

  const msgTime = document.createElement('div');
  msgTime.className = 'msg-time';
  msgTime.textContent = formatTime(new Date());

  div.appendChild(sender);
  div.appendChild(msgText);
  div.appendChild(msgTime);
  chatMessages.appendChild(div);

  if (isAtBottom) {
    chatMessages.scrollTop = chatMessages.scrollHeight;
  } else {
    unreadCount++;
    unreadBadge.textContent = String(unreadCount);
    unreadBadge.hidden = false;
    scrollBottomBtn.hidden = false;
  }
}

function appendSystemMessage(text: string): void {
  const div = document.createElement('div');
  div.className = 'chat-msg system';

  const msgText = document.createElement('div');
  msgText.className = 'msg-text';
  msgText.textContent = text;

  div.appendChild(msgText);
  chatMessages.appendChild(div);

  if (isAtBottom) {
    chatMessages.scrollTop = chatMessages.scrollHeight;
  }
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
document.addEventListener('keydown', (e) => {
  // Don't trigger shortcuts when typing in inputs
  if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;

  const key = e.key.toLowerCase();

  // PTT keys: Space and T (only in PTT mode)
  if (micMode === 'ptt' && (key === ' ' || key === 't') && !e.repeat) {
    e.preventDefault();
    pttActivate();
    return;
  }

  switch (key) {
    case 'm':
      if (micMode === 'open' && room && room.hasMedia) {
        room.toggleAudio().then(enabled => {
          updateMicButton(enabled);
          updateLocalTile();
        });
      }
      break;
    case 'v':
      if (room && room.hasMedia) {
        room.toggleVideo().then(enabled => {
          updateCamButton(enabled);
          updateLocalTile();
        });
      }
      break;
    case 's':
      if (room && room.hasMedia) {
        if (room.isScreenSharing) {
          room.stopScreenShare();
          updateScreenButton(false);
        } else {
          room.startScreenShare().then(success => {
            updateScreenButton(success);
          });
        }
      }
      break;
    case 'escape':
      settingsModal.hidden = true;
      roomSettingsModal.hidden = true;
      document.getElementById('mod-menu')?.remove();
      break;
  }
});

document.addEventListener('keyup', (e) => {
  if (e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement) return;
  const key = e.key.toLowerCase();
  if (micMode === 'ptt' && (key === ' ' || key === 't')) {
    pttDeactivate();
  }
});

// Release PTT if window loses focus while key is held
window.addEventListener('blur', () => {
  if (pttHeld) pttDeactivate();
});
