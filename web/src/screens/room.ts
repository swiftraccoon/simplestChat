import type { Elements } from '../dom';
import {
  RoomClient,
  RoomPasswordRequiredError,
  type Participant,
  type ConnectionQuality,
} from '../room';
import type { AuthManager } from '../auth';
import type { SignalingClient } from '../signaling';
import type { SocialChat } from '../social-chat';
import type { CommunityUI } from '../community-ui';
import type { ParticipantHovercard } from '../participant-hovercard';
import type { MediaControls } from '../media-controls';
import type { SettingsSheet } from '../settings-sheet';
import type { RoomSettingsScreen } from './room-settings';
import type { RoomLayout } from './room-layout';
import type { CallOutcomeTelemetry, MediaTelemetry } from '../media-telemetry';
import type { CallHealth } from '../call-health';
import type { ClientTelemetry } from '../telemetry';
import type { RoomNavigation } from '../room-navigation';
import type { Role } from '../protocol';
import * as icons from '../icons';
import { createScope } from '../disposable';
import { applyAppearance } from '../appearance';
import { avatarColors, chatColor } from '../avatar-colors';
import { button, el, modal } from '../ui';
import { markSaved, note, segmentRow } from '../settings-sheet';
import { createRoomMedia, type RoomPublishingState, type MicMode } from './room-media';

export interface RoomScreenStore {
  room: RoomClient | null;
  selectedRoomId: string;
  navigationPending: boolean;
  roomSelectionVersion: number;
  departureInProgress: Promise<void> | null;
}
export interface RoomScreenServices {
  readonly auth: AuthManager;
  readonly signaling: SignalingClient;
  readonly socialChat: SocialChat;
  readonly community: CommunityUI;
  readonly participantHovercard: ParticipantHovercard;
  readonly mediaControls: MediaControls;
  readonly settingsSheet: SettingsSheet;
  readonly roomSettingsScreen: RoomSettingsScreen;
  readonly roomLayout: RoomLayout;
  readonly callTelemetry: CallOutcomeTelemetry;
  readonly callHealth: CallHealth;
  readonly mediaTelemetry: MediaTelemetry;
  readonly telemetry: ClientTelemetry;
  readonly navigation: RoomNavigation;
  showToast(message: string, duration?: number, kind?: 'info' | 'error'): void;
  observeUiTask(task: Promise<unknown>, failureMessage: string): void;
  asyncUiAction(action: () => Promise<unknown>, failureMessage: string): () => void;
  showActionToast(
    message: string,
    actions: { label: string; action: () => void }[],
    duration?: number,
  ): HTMLElement;
  showRoomExitNotice(title: string, message: string): void;
  setRoomToolsVisible(visible: boolean): void;
  placeDiagnosticsButton(inRoom: boolean): void;
  updateJoinBtn(): void;
  readLocalPreference(key: string): string | null;
  writeLocalPreference(key: string, value: string): void;
  announceRoomEntry(roomName: string): void;
  setButtonContent(button: HTMLElement, iconHtml: string, tooltip?: string): void;
  refreshDirectory(): Promise<unknown>;
  setCallHealthVisible(visible: boolean): void;
  closeCallHealth(): void;
  dismissHomeDialogs(): void;
}

/** Owns the live room membership, moderation, roster and keyboard lifecycle. */
export function createRoomScreen(
  elements: Elements,
  store: RoomScreenStore,
  services: RoomScreenServices,
) {
  const scope = createScope();
  const {
    handBtn,
    roomSettingsBtn,
    chatInput,
    chatSendBtn,
    screenBtn,
    camBtn,
    lobbyTab,
    lobbyListEl,
    lobbyEmpty,
    joinBtn,
    nameInput,
    roomInput,
    participantList,
    roomLabel,
    roomTopic,
    joinScreen,
    roomScreen,
    lobbyScreen,
    lobbyRoomName,
    lobbyTopic,
    connectionStatus,
    micBtn,
    qualityIndicator,
    lobbyCount,
    lobbyCancelBtn,
    videoGrid,
    chatMessages,
    unreadBadge,
    scrollBottomBtn,
    leaveBtn,
    homeLink,
    settingsBtn,
    usersPanel,
  } = elements;
  function listen<K extends keyof HTMLElementEventMap>(
    target: HTMLElement | Document | Window,
    type: K,
    listener: (event: HTMLElementEventMap[K]) => void,
    options: AddEventListenerOptions = {},
  ): void {
    target.addEventListener(type, listener as EventListener, { ...options, signal: scope.signal });
  }
  const personalMicMode = services.readLocalPreference('micMode') === 'ptt' ? 'ptt' : 'open';
  const publishing: RoomPublishingState = {
    personalMicMode,
    micMode: personalMicMode,
    pttHeld: false,
    pttActivation: 0,
    cameraTogglePending: false,
    microphoneTogglePending: false,
  };
  const media = createRoomMedia(elements, store, services, publishing, {
    get recovering() {
      return roomRecovering;
    },
    get followedSpeaker() {
      return followedSpeaker;
    },
    get talkRow() {
      return talkRow;
    },
    showModerationMenu,
  });
  function clearChildren(element: HTMLElement): void {
    element.replaceChildren();
  }

  let roomPasswordView: { cancel: () => void } | null = null;

  // --- Settings sheet ---
  let talkRow: ReturnType<typeof segmentRow> | null = null;

  let talkNote: HTMLElement | null = null;

  /** The talk mode row inside Camera & mic; a room that requires hold-to-talk locks it. */
  function buildTalkRow(): HTMLElement {
    const wrapper = document.createElement('div');
    wrapper.className = 'sheet-group';
    const row = segmentRow(
      'Talk',
      [
        { value: 'open', label: 'Open mic', id: 'talk-open' },
        { value: 'ptt', label: 'Hold to talk', id: 'talk-ptt' },
      ],
      publishing.micMode,
      (value) => {
        media.setMicMode(value as MicMode);
        markSaved(row.row);
      },
    );
    talkRow = row;
    talkNote = note('This room requires hold to talk.');
    wrapper.append(row.row, talkNote);
    syncTalkRow();
    return wrapper;
  }

  function syncTalkRow(): void {
    if (!talkRow) return;
    const locked = store.room?.roomSettings?.pushToTalk === true;
    for (const choice of talkRow.buttons) choice.disabled = locked;
    talkRow.set(publishing.micMode);
    if (talkNote) talkNote.hidden = !locked;
  }

  let localTextMuted = false;

  let roomRecovering = false;

  const lobbyWaiters = new Map<string, string>();

  // participantId → displayName
  const lobbyActions = new Map<string, { pending: boolean; error?: string }>();

  // Active speaker / audio level tracking — avoids querySelectorAll on every event
  const AUDIO_LEVEL_THRESHOLD = -50;

  // dB; only highlight above this
  // The server reports levels every 800 ms while anyone speaks and sends an
  // empty list on silence; the timer covers a lost silence report.
  const SPEAKING_HIGHLIGHT_TIMEOUT_MS = 2000;

  let currentDominantTile: HTMLDivElement | null = null;

  let followedSpeaker: string | null = null;

  const currentlySpeaking = new Set<HTMLElement>();

  // tiles + list items with .speaking
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

  // --- Moderation Context Menu ---
  function showModerationMenu(targetId: string, targetName: string, x: number, y: number): void {
    services.participantHovercard.close();
    document.getElementById('mod-menu')?.remove();

    const role = store.room?.role ?? 'user';
    const isMod = role === 'owner' || role === 'admin' || role === 'moderator';
    const isAdmin = role === 'owner' || role === 'admin';

    const owner = store.room;
    if (!owner) return;
    const membership = owner.membershipVersion;
    const items: { label: string; action: () => void | Promise<void>; danger?: boolean }[] = [];

    const isSelf = targetId === store.room?.localParticipantId;
    const authenticated = isSelf
      ? services.auth.isLoggedIn
      : store.room?.getParticipants().get(targetId)?.authenticated === true;
    if (authenticated)
      items.push({
        label: 'View profile',
        action: () =>
          services.observeUiTask(
            services.community.showProfile(targetId),
            'Could not open the profile',
          ),
      });
    if (!isSelf) {
      items.push({
        label: 'Private message',
        action: () => services.socialChat.openPrivate(targetId, targetName),
      });
      items.push({
        label: services.socialChat.isIgnored(targetId) ? 'Unignore messages' : 'Ignore messages',
        action: () => {
          services.socialChat
            .toggleIgnore(targetId, targetName, authenticated)
            .catch((error) =>
              services.showToast(error instanceof Error ? error.message : 'Ignore update failed'),
            );
        },
      });
      items.push({
        label: 'Report to moderators',
        action: () => services.community.report(targetId, targetName),
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
      items.push({
        label: 'Ban…',
        action: () => services.community.ban(targetId, targetName),
        danger: true,
      });
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
      if (
        scope.signal.aborted ||
        pending ||
        store.room !== owner ||
        owner.membershipVersion !== membership
      )
        return;
      pending = true;
      error.hidden = true;
      menu.setAttribute('aria-busy', 'true');
      for (const button of menu.querySelectorAll('button')) button.disabled = true;
      try {
        await action();
        menu.remove();
      } catch (failure) {
        if (
          !scope.signal.aborted &&
          store.room === owner &&
          owner.membershipVersion === membership
        ) {
          const message =
            failure instanceof Error ? failure.message : 'The action was not confirmed';
          if (menu.isConnected) {
            error.textContent = message;
            error.hidden = false;
          } else services.showToast(message, 7000);
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
        services.asyncUiAction(() => apply(item.action), 'Could not apply room action'),
      );
      menu.appendChild(btn);
    }

    // Set Role sub-menu
    const roleOptions: { label: string; value: Role }[] = [];
    if (isMod && !isSelf) {
      roleOptions.push({ label: 'User', value: 'user' });
      roleOptions.push({ label: 'Member', value: 'member' });
    }
    if (isAdmin && !isSelf) {
      roleOptions.push({ label: 'Moderator', value: 'moderator' });
    }
    if (role === 'owner' && !isSelf) {
      roleOptions.push({ label: 'Admin', value: 'admin' });
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
          services.asyncUiAction(
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
    setTimeout(() => {
      if (!scope.signal.aborted)
        document.addEventListener('click', close, { signal: scope.signal });
    }, 0);
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

  function getRoleBadgeSpan(role: Role): HTMLSpanElement | null {
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
    const role = store.room?.role ?? 'user';
    const settings = store.room?.roomSettings;

    // Show hand-raise button for non-privileged users in moderated rooms
    const isPrivileged =
      role === 'owner' || role === 'admin' || role === 'moderator' || role === 'member';
    handBtn.hidden = !(settings?.moderated && !isPrivileged);

    // Show room settings button for admins+
    roomSettingsBtn.hidden = !(role === 'owner' || role === 'admin');

    renderLobbyPanel();
  }

  function applyRoomSettingsToUI(): void {
    const settings = store.room?.roomSettings;
    const privileged = ['member', 'moderator', 'admin', 'owner'].includes(
      store.room?.role ?? 'guest',
    );
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
    chatSendBtn.disabled = Boolean(chatDisabled);
    chatSendBtn.classList.toggle('disabled', Boolean(chatDisabled));

    media.setMicMode(settings?.pushToTalk ? 'ptt' : publishing.personalMicMode, false);
    syncTalkRow();

    // Screen sharing: hide button when disabled
    if (settings?.allowScreenSharing === false) {
      screenBtn.hidden = true;
    } else {
      screenBtn.hidden = !navigator.mediaDevices?.getDisplayMedia;
    }

    // Video: hide cam button when disabled
    camBtn.hidden = settings?.allowVideo === false;
    services.socialChat.participantsChanged();
  }

  function renderLobbyPanel(): void {
    const role = store.room?.role ?? 'user';
    const lobbyEnabled =
      store.room?.roomSettings?.lobbyEnabled ||
      store.room?.roomSettings?.inviteOnly ||
      lobbyWaiters.size > 0;
    const canAdmit = role === 'owner' || role === 'admin' || role === 'moderator';

    // Show/hide lobby tab
    lobbyTab.hidden = !(canAdmit && lobbyEnabled);
    if (lobbyTab.hidden && lobbyTab.classList.contains('active'))
      services.roomLayout.selectTab('chat');

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
        services.asyncUiAction(() => applyLobbyAction(id, 'admit'), 'Could not admit participant'),
      );

      const denyBtn = document.createElement('button');
      denyBtn.className = 'lobby-deny-btn';
      denyBtn.textContent = 'Deny';
      denyBtn.addEventListener(
        'click',
        services.asyncUiAction(() => applyLobbyAction(id, 'deny'), 'Could not deny participant'),
      );

      const state = lobbyActions.get(id);
      admitBtn.disabled = Boolean(state?.pending) || !store.room?.connected;
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
    const owner = store.room;
    if (!owner || lobbyActions.get(id)?.pending) return;
    const membership = owner.membershipVersion;
    const current = () =>
      !scope.signal.aborted && store.room === owner && owner.membershipVersion === membership;
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
          services.showToast(message);
        }
      }
    } finally {
      if (current()) renderLobbyPanel();
    }
  }

  /** Owned, masked, disposable prompt; closing resolves cancellation exactly once. */
  function requestRoomPassword(owner: RoomClient, reconnecting = false): Promise<string | null> {
    if (scope.signal.aborted || store.room !== owner) return Promise.resolve(null);
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
          resolve(store.room === owner && owner.membershipVersion === membership ? answer : null);
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
      if (scope.signal.aborted || store.room !== owner) return null;
      if (!(error instanceof RoomPasswordRequiredError)) throw error;
      const membership = owner.membershipVersion;
      const password = await requestRoomPassword(owner);
      if (scope.signal.aborted || store.room !== owner || owner.membershipVersion !== membership)
        return null;
      if (password === null) {
        await leaveCurrentRoom();
        return null;
      }
      return owner.join(roomId, name, password);
    }
  }

  listen(
    joinBtn,
    'click',
    services.asyncUiAction(async () => {
      const name = nameInput.value.trim();
      const roomId = roomInput.value.trim();
      if (
        !name ||
        !roomId ||
        store.room ||
        store.departureInProgress ||
        store.navigationPending ||
        !services.navigation.join(roomId)
      )
        return;

      store.selectedRoomId = roomId;
      services.writeLocalPreference('displayName', name);

      joinBtn.disabled = true;
      joinBtn.textContent = 'Joining...';
      localTextMuted = false;
      roomRecovering = false;

      let joiningRoom: RoomClient | null = null;
      const ownsCallbacks = (): boolean =>
        !scope.signal.aborted && joiningRoom !== null && store.room === joiningRoom;
      function ownedCallback<Args extends unknown[]>(
        callback: (...args: Args) => void,
      ): (...args: Args) => void {
        return (...args) => {
          if (ownsCallbacks()) callback(...args);
        };
      }
      try {
        roomPasswordView?.cancel();
        store.room = new RoomClient(services.signaling, {
          onTelemetry: ownedCallback(services.telemetry.record),
          onCallSignal: ownedCallback((signal) => {
            if (store.room === joiningRoom) services.callTelemetry.signal(signal);
          }),
          onBackgroundError: ownedCallback((message) => services.showToast(message)),
          onParticipantsChanged: ownedCallback((participants) => {
            services.community.refresh();
            services.observeUiTask(
              services.socialChat.activate(),
              'Could not refresh room conversations',
            );
            renderParticipants(participants);
            services.socialChat.participantsChanged();
          }),
          onLocalStream: ownedCallback(() => {}), // Unused — local tile managed by updateLocalTile on user action
          onLocalMediaChanged: ownedCallback(() => {
            if (!store.room) return;
            media.updateMicButton(store.room.audioEnabled);
            media.updateCamButton(store.room.videoEnabled);
            media.updateScreenButton(store.room.isScreenSharing);
            media.updateLocalTile();
          }),
          onLocalCaptureStopped: ownedCallback(media.handleLocalCaptureStopped),
          onLocalVideoStalled: ownedCallback(media.handleLocalVideoStalled),
          onRemoteTrack: ownedCallback(media.renderRemoteTrack),
          onRemoteTrackRemoved: ownedCallback(media.removeRemoteTrack),
          onRemoteVideoDeferred: ownedCallback(media.renderDeferredVideo),
          onRemoteMediaUnavailable: ownedCallback(
            (_participantId, participantName, kind, _source, reason) => {
              services.showToast(`Cannot receive ${participantName}'s ${kind}: ${reason}`, 6000);
            },
          ),
          onParticipantLeft: ownedCallback(handleParticipantLeft),
          onChatMessage: ownedCallback(() => {}), // Typed entries and delivery state handled by session inbox.
          onSocialEvent: ownedCallback((message) => {
            services.socialChat.handleEvent(message);
            if (message.type === 'nicknameChanged') {
              if (message.participantId === store.room?.localParticipantId)
                nameInput.value = message.nickname;
              for (const [key, tile] of media.tiles) {
                if (tile.dataset['participantId'] !== message.participantId) continue;
                const tag = tile.querySelector('.name-tag');
                if (tag)
                  tag.textContent = message.nickname + (key.endsWith(':screen') ? ' (Screen)' : '');
              }
              // An automatic color follows the name.
              media.repaintTiles(message.participantId);
            } else if (message.type === 'chatStyleChanged') {
              media.repaintTiles(message.participantId);
            } else if (message.type === 'socialResponse' && message.action === 'getRoomSnapshot') {
              const lobby = message.data['lobby'] as
                { participantId: string; displayName: string }[] | undefined;
              lobbyWaiters.clear();
              for (const waiter of lobby ?? [])
                lobbyWaiters.set(waiter.participantId, waiter.displayName);
              renderLobbyPanel();
            }
          }),
          onConnectionQuality: ownedCallback(renderConnectionQuality),
          onParticipantJoined: ownedCallback(handleParticipantJoined),
          onFollowedSpeakerChanged: ownedCallback((participantId) => {
            followedSpeaker = participantId;
            media.orderVideoTiles();
          }),
          onActiveSpeaker: ownedCallback((participantId) => {
            if (currentDominantTile) {
              currentDominantTile.classList.remove('dominant-speaker');
              currentDominantTile = null;
            }
            const tile = media.tiles.get(participantId);
            if (tile) {
              tile.classList.add('dominant-speaker');
              currentDominantTile = tile;
            }
          }),
          onAudioLevels: ownedCallback((levels) => {
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
                const tile = media.tiles.get(participantId);
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
          }),
          onModeration: ownedCallback((action, participantId, reason) => {
            const isLocal = participantId === store.room?.localParticipantId;
            if (isLocal && (action === 'kicked' || action === 'banned')) {
              services.observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
              services.showRoomExitNotice(
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
            const participants = store.room?.getParticipants();
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
            services.showToast(messages[action] ?? `${action} on ${targetName}`);
          }),
          onRoleChanged: ownedCallback((participantId, newRole) => {
            services.community.refresh();
            services.socialChat.participantsChanged();
            const participants = store.room?.getParticipants();
            const targetName =
              participants?.get(participantId)?.name ??
              (participantId === store.room?.localParticipantId
                ? store.room.nickname
                : participantId.slice(0, 8));
            if (participantId === store.room?.localParticipantId) {
              services.showToast(`Your role has been changed to ${newRole}`);
              if (newRole !== 'owner' && newRole !== 'admin' && services.roomSettingsScreen.isOpen)
                services.settingsSheet.close({ force: true });
              services.settingsSheet.refresh();
              handBtn.classList.remove('hand-raised');
            } else {
              services.showToast(`${targetName} is now ${newRole}`);
            }
            updateRoomModeUI();
            applyRoomSettingsToUI();
            // Re-render participants to update role badges
            if (store.room) renderParticipants(store.room.getParticipants());
          }),
          onRoomSettingsChanged: ownedCallback((settings) => {
            roomLabel.textContent = settings.displayName;
            applyAppearance(roomLabel, settings.nameStyle, settings.displayName);
            applyAppearance(roomTopic, settings.topicStyle, settings.topic ?? '');
            services.socialChat.participantsChanged();
            updateRoomModeUI();
            applyRoomSettingsToUI();
            services.roomSettingsScreen.sync();
            services.settingsSheet.refresh();
            services.socialChat.refreshSavedChat();
            roomTopic.textContent = settings.topic ?? '';
            roomTopic.hidden = !settings.topic;
          }),
          onTopicChanged: ownedCallback((topic, changedBy) => {
            roomTopic.textContent = topic;
            applyAppearance(roomTopic, store.room?.roomSettings?.topicStyle, topic);
            roomTopic.hidden = !topic;
            services.showToast(`Topic changed by ${changedBy}: ${topic}`);
          }),
          onVoiceRequested: ownedCallback((participantId, displayName) => {
            const role = store.room?.role ?? 'user';
            const canGrant = role === 'owner' || role === 'admin' || role === 'moderator';
            if (canGrant) {
              services.showActionToast(`${displayName} is requesting voice`, [
                {
                  label: 'Grant',
                  action: () => {
                    const owner = store.room;
                    if (owner)
                      services.observeUiTask(
                        owner.setRole(participantId, 'member'),
                        'Could not grant voice',
                      );
                  },
                },
                { label: 'Dismiss', action: () => {} },
              ]);
            } else {
              services.showToast(`${displayName} is requesting voice`);
            }
          }),
          onLobbyWaiting: ownedCallback((roomName, topic, count, moderators) => {
            services.mediaControls.reset();
            services.setRoomToolsVisible(false);
            joinScreen.hidden = true;
            roomScreen.hidden = true;
            lobbyScreen.hidden = false;
            lobbyRoomName.textContent = roomName;
            lobbyTopic.textContent = topic ?? '';
            lobbyTopic.hidden = !topic;
            updateLobbyStatus(count, moderators);
          }),
          onLobbyStatus: ownedCallback(updateLobbyStatus),
          onLobbyJoin: ownedCallback((participantId, displayName) => {
            lobbyWaiters.set(participantId, displayName);
            renderLobbyPanel();
            services.showToast(`${displayName} is waiting in the lobby`);
          }),
          onLobbyAdmitted: ownedCallback(() => {
            lobbyScreen.hidden = true;
            joinScreen.hidden = true;
            roomScreen.hidden = false;
            applyJoinedRoomUI();
            services.announceRoomEntry(
              store.room?.roomSettings?.displayName ?? store.room?.currentRoomId ?? 'the room',
            );
          }),
          onAdmissionComplete: ownedCallback(() => {
            // Post-admission media + room state are ready — refresh buttons/settings UI
            applyJoinedRoomUI();
          }),
          onLobbyDenied: ownedCallback((reason) => {
            services.observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
            services.showRoomExitNotice(
              'Entry declined',
              `A moderator declined your request to enter.${reason ? ` ${reason}` : ''}`,
            );
          }),
          onRecoveryState: ownedCallback((state, message) => {
            if (state === 'reconnecting') services.socialChat.prepareSnapshotRecovery();
            document.getElementById('room-recovery-notice')?.remove();
            roomRecovering = state !== 'connected';
            if (state !== 'connected') services.roomSettingsScreen.retire();
            if (state === 'connected') {
              connectionStatus.textContent = 'Connected';
              connectionStatus.className = 'status connected';
              if (store.room?.localParticipantId) {
                applyJoinedRoomUI();
                media.updateLocalTile();
              }
              services.showToast(message ?? 'Room connection restored');
            } else {
              connectionStatus.textContent =
                state === 'reconnecting' ? 'Rejoining room…' : 'Room recovery failed';
              connectionStatus.className = `status ${state === 'reconnecting' ? 'connecting' : 'disconnected'}`;
              media.pttDeactivate();
              applyRoomSettingsToUI();
              if (state === 'failed') {
                const recoveringRoom = store.room;
                const notice = services.showActionToast(
                  message ?? 'Unable to rejoin the room',
                  [
                    {
                      label: 'Retry connection',
                      action: () => {
                        if (store.room === recoveringRoom) recoveringRoom?.retryRecovery();
                      },
                    },
                    {
                      label: 'Leave room',
                      action: () => {
                        if (store.room === recoveringRoom)
                          services.observeUiTask(
                            leaveCurrentRoom(),
                            'Could not finish leaving the room',
                          );
                      },
                    },
                  ],
                  0,
                );
                notice.id = 'room-recovery-notice';
              }
            }
          }),
          onPasswordRequired: () =>
            ownsCallbacks()
              ? (() =>
                  joiningRoom ? requestRoomPassword(joiningRoom, true) : Promise.resolve(null))()
              : Promise.resolve(null),
          onRoomClosed: ownedCallback((reason) => {
            services.observeUiTask(leaveCurrentRoom(), 'Could not finish leaving the room');
            services.showToast(reason, 8000);
          }),
        });

        store.room.preferChatStyle(services.socialChat.savedLook());
        joiningRoom = store.room;
        const status = await joinRoomWithPassword(joiningRoom, roomId, name);
        if (scope.signal.aborted || store.room !== joiningRoom || status === null) return;

        if (status === 'lobby') {
          // onLobbyWaiting already switched to the lobby screen. Room UI is applied
          // by onLobbyAdmitted/onAdmissionComplete if/when we are admitted.
          return;
        }

        joinScreen.hidden = true;
        roomScreen.hidden = false;
        applyJoinedRoomUI();
        services.announceRoomEntry(joiningRoom.roomSettings?.displayName ?? roomId);
      } catch (e) {
        if (scope.signal.aborted || store.room !== joiningRoom) return;
        await leaveCurrentRoom();
        if (store.room) return;
        reportJoinFailure(e);
      }
    }, 'Could not complete joining the room'),
  );

  function reportJoinFailure(error: unknown): void {
    console.error('Failed to join:', error);
    services.showRoomExitNotice(
      'Could not join',
      error instanceof Error ? error.message : String(error),
    );
    joinBtn.textContent = 'Join Room';
    services.updateJoinBtn();
  }

  /** Apply all in-room UI state (label, topic, control buttons, settings-driven UI).
   * Called on direct join, on lobby admission (media pending), and again once
   * post-admission media setup completes. Idempotent. */
  function applyJoinedRoomUI(): void {
    if (!store.room) return;
    services.roomLayout.setLayout(services.roomLayout.layout);

    roomLabel.textContent = store.room.roomSettings?.displayName ?? store.room.currentRoomId ?? '';
    applyAppearance(roomLabel, store.room.roomSettings?.nameStyle, roomLabel.textContent);
    applyAppearance(
      roomTopic,
      store.room.roomSettings?.topicStyle,
      store.room.roomSettings?.topic ?? '',
    );
    roomLabel.hidden = false;
    services.setRoomToolsVisible(true);
    services.placeDiagnosticsButton(true);

    const topic = store.room.roomSettings?.topic;
    roomTopic.textContent = topic ?? '';
    roomTopic.hidden = !topic;

    // Reflect retained media on reconnect; fresh room sessions start without capture.
    if (store.room.hasMedia) {
      micBtn.style.opacity = '';
      camBtn.style.opacity = '';
      micBtn.classList.remove('muted');
      camBtn.classList.remove('muted');
      media.updateMicButton(store.room.audioEnabled);
      media.updateCamButton(store.room.videoEnabled);
      media.updateScreenButton(store.room.isScreenSharing);
      // Register callback so UI updates when browser's "Stop sharing" button is clicked
      const owner = store.room;
      const membership = owner.membershipVersion;
      store.room.onScreenShareStopped = () => {
        if (!scope.signal.aborted && store.room === owner && owner.membershipVersion === membership)
          media.updateScreenButton(false);
      };
      store.room.onScreenShareAudioChanged = () => {
        if (!scope.signal.aborted && store.room === owner && owner.membershipVersion === membership)
          media.updateScreenButton(owner.isScreenSharing);
      };
    } else {
      // Media unavailable (or not yet set up while waiting in lobby)
      console.warn('[ui] media not available, buttons will be non-functional');
      services.setButtonContent(micBtn, icons.micOff(), 'No mic access');
      micBtn.classList.add('muted');
      micBtn.style.opacity = '0.4';
      services.setButtonContent(camBtn, icons.camOff(), 'No cam access');
      camBtn.classList.add('muted');
      camBtn.style.opacity = '0.4';
    }

    qualityIndicator.hidden = false;
    services.setCallHealthVisible(true);
    services.callHealth.sample();
    updateRoomModeUI();
    applyRoomSettingsToUI();
    services.observeUiTask(services.socialChat.activate(), 'Could not refresh room conversations');
    services.community.refresh();
  }

  let roomTopicView: ReturnType<typeof modal> | null = null;

  // Every participant can read the full topic; editing still uses room permissions.
  listen(roomTopic, 'click', () => {
    if (!store.room || !roomTopic.textContent) return;
    const activeRoom = store.room;
    const membership = store.room.membershipVersion;
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
    if (store.room.role === 'owner' || store.room.role === 'admin') {
      view.body.append(
        button('Edit topic', () => {
          view.close();
          if (
            store.room === activeRoom &&
            store.room.membershipVersion === membership &&
            (store.room.role === 'owner' || store.room.role === 'admin')
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
  listen(lobbyCancelBtn, 'click', () => services.navigation.home());

  // --- Leave ---

  function leaveCurrentRoom(): Promise<void> {
    if (scope.signal.aborted) return Promise.resolve();
    store.departureInProgress ??= leaveRoomAndShowHome().finally(() => {
      store.departureInProgress = null;
      if (scope.signal.aborted) return;
      services.updateJoinBtn();
      services.navigation.resumePendingJoin();
    });
    services.updateJoinBtn();
    return store.departureInProgress;
  }

  async function leaveRoomAndShowHome(): Promise<void> {
    services.participantHovercard.reset();
    services.roomSettingsScreen.retire();
    roomPasswordView?.cancel();
    roomPasswordView = null;
    roomTopicView?.close();
    document.getElementById('room-recovery-notice')?.remove();
    media.pttDeactivate();
    services.mediaControls.reset();
    const leavingRoom = store.room;
    store.room = null;
    services.socialChat.reset();
    services.community.refresh();
    await leavingRoom?.leave();
    if (scope.signal.aborted) return;
    // An explicit departure also makes the homepage usable after a bounded
    // recovery failure, without changing the retained account identity.
    if (services.signaling.reconnectExhausted) services.signaling.retryConnection();
    localTextMuted = false;
    roomRecovering = false;
    media.setMicMode(publishing.personalMicMode, false);
    syncTalkRow();

    followedSpeaker = null;
    clearChildren(videoGrid);
    clearChildren(participantList);
    clearChildren(chatMessages);
    unreadBadge.hidden = true;
    scrollBottomBtn.hidden = true;
    media.resetTiles();
    lobbyWaiters.clear();
    lobbyActions.clear();

    // Remove classic users panel if present
    document.getElementById('classic-users-panel')?.remove();

    // Reset speaking/active-speaker tracking
    clearSpeakingHighlights();

    // Reset PTT state
    publishing.pttHeld = false;
    publishing.pttActivation++;

    // Reset mic/cam button states (clear inline opacity from no-media mode)
    micBtn.style.opacity = '';
    camBtn.style.opacity = '';
    micBtn.classList.remove('active', 'muted', 'ptt-active');
    camBtn.classList.remove('active', 'muted');
    media.updateScreenButton(false);
    handBtn.hidden = true;
    handBtn.classList.remove('hand-raised');
    roomSettingsBtn.hidden = true;
    services.setButtonContent(micBtn, icons.micOn(), 'Mic (M)');
    services.setButtonContent(camBtn, icons.camOn(), 'Cam (V)');
    services.setButtonContent(screenBtn, icons.screenShare(), 'Screen (S)');
    // Remove PTT label if present
    micBtn.querySelector('.ptt-label')?.remove();

    // Reset settings-driven UI
    chatInput.disabled = false;
    chatSendBtn.disabled = false;
    chatInput.placeholder = 'Type a message...';
    chatSendBtn.classList.remove('disabled');
    screenBtn.hidden = !navigator.mediaDevices?.getDisplayMedia;
    camBtn.hidden = false;

    roomScreen.hidden = true;
    services.setRoomToolsVisible(false);
    lobbyScreen.hidden = true;
    joinScreen.hidden = false;
    services.placeDiagnosticsButton(false);
    roomLabel.hidden = true;
    roomTopic.hidden = true;
    roomTopic.textContent = '';
    joinBtn.textContent = 'Join Room';
    qualityIndicator.hidden = true;
    qualityIndicator.className = 'quality-dot';
    services.setCallHealthVisible(false);
    services.closeCallHealth();
    services.callHealth.sample();
    services.settingsSheet.close({ force: true });
    document.getElementById('mod-menu')?.remove();
    services.updateJoinBtn();
    services.observeUiTask(services.refreshDirectory(), 'Could not refresh the room directory');
  }

  listen(leaveBtn, 'click', () => services.navigation.home());

  listen(homeLink, 'click', (event) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)
      return;
    event.preventDefault();
    services.navigation.home();
    homeLink.focus();
  });

  // --- Hand raise ---
  let voiceRequestPending = false;

  listen(
    handBtn,
    'click',
    services.asyncUiAction(async () => {
      const owner = store.room;
      if (!owner || voiceRequestPending || handBtn.classList.contains('hand-raised')) return;
      const membership = owner.membershipVersion;
      voiceRequestPending = true;
      handBtn.setAttribute('aria-busy', 'true');
      try {
        await owner.requestVoice();
        if (scope.signal.aborted || store.room !== owner || membership !== owner.membershipVersion)
          return;
        handBtn.classList.add('hand-raised');
        services.showToast('Voice request sent to moderators');
      } finally {
        voiceRequestPending = false;
        handBtn.setAttribute('aria-busy', 'false');
      }
    }, 'Could not request voice'),
  );

  // --- Room settings (the sheet's This room screens) ---
  listen(roomSettingsBtn, 'click', () => {
    if (services.roomSettingsScreen.pending) {
      services.showToast('A room change is still awaiting confirmation');
      return;
    }
    services.settingsSheet.open({ segment: 'room', opener: roomSettingsBtn });
  });

  // --- Personal settings ---
  listen(settingsBtn, 'click', () => {
    services.settingsSheet.open({ opener: settingsBtn });
  });

  // --- Rendering ---
  /** Reconcile only the visible roster; unchanged rows retain focus and their avatars. */
  function renderParticipants(participants: Map<string, Participant>): void {
    services.participantHovercard.refresh();
    const all = Array.from(participants.values());
    const localId = store.room?.localParticipantId;
    if (localId && store.room) {
      const producers = new Map<string, { kind: 'audio' | 'video'; source?: string }>();
      if (store.room.audioEnabled) producers.set('local-audio', { kind: 'audio' });
      if (store.room.videoEnabled) producers.set('local-video', { kind: 'video' });
      all.push({
        id: localId,
        name: store.room.nickname || nameInput.value.trim(),
        role: store.room.role,
        authenticated: services.auth.isLoggedIn,
        ...(store.room.chatStyle && { chatStyle: store.room.chatStyle }),
        producers,
      });
    }
    sortParticipantRoster(all);
    services.community.retainProfiles(
      all.filter((person) => person.authenticated).map((person) => person.id),
    );
    const classic = services.roomLayout.layout === 'classic' && services.roomLayout.desktop;
    let panel = document.getElementById('classic-users-panel');
    const visible = classic
      ? !services.roomLayout.preferences.rosterCollapsed
      : usersPanel.classList.contains('active') &&
        !(services.roomLayout.desktop
          ? services.roomLayout.preferences.chatCollapsed
          : services.roomLayout.mobileCollapsed);
    if (!visible) {
      clearChildren(participantList);
      if (classic) panel?.querySelector('.classic-user-list')?.replaceChildren();
      else panel?.remove();
      services.participantHovercard.refresh();
      return;
    }
    let list: HTMLElement = participantList;
    if (classic) {
      clearChildren(participantList);
      if (!panel) {
        panel = el('aside');
        panel.id = 'classic-users-panel';
        panel.classList.add('classic-users-panel');
        panel.append(el('ul', undefined, 'classic-user-list'));
        roomScreen.insertBefore(panel, roomScreen.firstChild);
        services.roomLayout.attachPanelResize(panel, 'roster');
        services.roomLayout.apply();
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
      document.activeElement?.closest<HTMLElement>('[data-participant-id]')?.dataset[
        'participantId'
      ];
    let position = list.firstChild;
    for (const p of all) {
      const local = p.id === localId;
      const hasAudio = Array.from(p.producers.values()).some(
        (producer) => producer.kind === 'audio',
      );
      const hasVideo = Array.from(p.producers.values()).some(
        (producer) => producer.kind === 'video',
      );
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
        services.participantHovercard.bind(name, p.id, p.name);
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
      services.community.decorateAvatar(
        row.querySelector<HTMLElement>('.participant-avatar')!,
        p.id,
        p.authenticated === true,
      );
    }
    for (const node of Array.from(list.children))
      if (!retained.has(node as HTMLElement)) node.remove();
    services.participantHovercard.refresh();
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

  function handleParticipantLeft(participantId: string, participantName?: string): void {
    services.mediaControls.detachParticipant(participantId);
    lobbyWaiters.delete(participantId);
    const tile = media.tiles.get(participantId);
    if (tile) {
      media.stopObservingTileSize(participantId);
      media.visibility?.unobserve(tile);
      tile.remove();
      media.tiles.delete(participantId);
    }
    // Also clean up screen share tile if present
    const screenTile = media.tiles.get(`${participantId}:screen`);
    if (screenTile) {
      media.visibility?.unobserve(screenTile);
      screenTile.remove();
      media.tiles.delete(`${participantId}:screen`);
    }
    media.updateVideoGridCount();
    if (participantName) appendSystemMessage(`${participantName} left`);
  }

  function handleParticipantJoined(participantId: string, participantName: string): void {
    lobbyWaiters.delete(participantId);
    renderLobbyPanel();
    appendSystemMessage(`${participantName} joined`);
  }

  function appendSystemMessage(text: string): void {
    services.socialChat.system(text);
  }

  function renderConnectionQuality(_quality: ConnectionQuality): void {
    // A server bandwidth estimate alone cannot establish local call health.
    services.callHealth.sample();
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
    if (!store.room || roomScreen.hidden || roomRecovering) return true;
    if (document.querySelector('dialog[open], .modal-overlay:not([hidden]), #mod-menu'))
      return true;
    return (
      event.target instanceof Element &&
      Boolean(
        event.target.closest(
          'input, textarea, select, button, a, summary, [contenteditable]:not([contenteditable="false"]), [role="textbox"], [role="separator"], [role="dialog"]',
        ),
      )
    );
  }

  listen(document, 'keydown', (e) => {
    const key = e.key.toLowerCase();
    if (key === 'escape') {
      // Native dialogs handle Escape themselves, including preview cleanup and focus restoration.
      if (document.querySelector('dialog[open], [popover]:popover-open')) return;
      services.dismissHomeDialogs();
      document.getElementById('mod-menu')?.remove();
      return;
    }
    if (shortcutsBlocked(e)) return;

    // PTT keys: Space and T (only in PTT mode)
    if (publishing.micMode === 'ptt' && (key === ' ' || key === 't') && !e.repeat) {
      e.preventDefault();
      if (media.canStartBroadcast('microphone'))
        services.observeUiTask(media.pttActivate(), 'Could not enable push-to-talk');
      return;
    }

    switch (key) {
      case 'm':
        services.observeUiTask(media.toggleMicrophone(), 'Could not update the microphone');
        break;
      case 'v':
        services.observeUiTask(media.toggleCamera(), 'Could not update the camera');
        break;
      case 's':
        services.observeUiTask(media.toggleScreenShare(), 'Could not update screen sharing');
        break;
    }
  });

  listen(document, 'keyup', (e) => {
    const key = e.key.toLowerCase();
    if (publishing.micMode === 'ptt' && (key === ' ' || key === 't')) {
      media.pttDeactivate();
    }
  });

  // Release PTT if window loses focus while key is held
  listen(window, 'blur', () => {
    if (publishing.pttHeld) media.pttDeactivate();
  });
  scope.onDispose(() => {
    roomPasswordView?.cancel();
    roomTopicView?.close();
    clearSpeakingHighlights();
    document.getElementById('mod-menu')?.remove();
    media.dispose();
  });
  return {
    renderParticipants,
    repaintTiles: media.repaintTiles,
    buildTalkRow,
    syncTalkRow,
    leaveCurrentRoom,
    showModerationMenu,
    shortcutsBlocked,
    controls: media,
    joinRoomWithPassword,
    requestRoomPassword,
    applyJoinedRoomUI,
    handleParticipantLeft,
    handleParticipantJoined,
    getRoleBadgeSpan,
    roleName: (role: Role): string => ROLE_NAMES[role] ?? role,
    get recovering() {
      return roomRecovering;
    },
    mediaElements: media.mediaElements,
    playbackElements: media.playbackElements,
    refreshIncomingMedia: media.refreshIncomingMedia,
    get canRefreshIncomingMedia() {
      return media.canRefreshIncomingMedia;
    },
    dispose: () => scope.dispose(),
  };
}
export type RoomScreen = ReturnType<typeof createRoomScreen>;
