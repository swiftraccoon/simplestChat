import type { Elements } from './dom';
import { createScope, type Disposable, type Scope } from './disposable';
import {
  createLocalPreferences,
  forgetThisDevice,
  signOutAndForget as forgetSignIn,
} from './local-preferences';
import { createDirectoryEntry, roomTone } from './room-entry';
import { createAppUpdateNotice } from './app-updates-ui';
import { createHomeScreen } from './screens/home';
import { createToasts } from './toasts';
import { createUiTasks } from './ui-tasks';
import { mountChrome } from './screens/chrome';
import { createRoomScreen as mountRoomScreen } from './screens/room';
import { createRoomLayout } from './screens/room-layout';
import { createInvitations } from './screens/invitations';
import { createAppSettings } from './screens/settings';
import { DirectoryScreen } from './screens/directory';
import { CreateRoomScreen } from './screens/create-room';
import { SignalingClient } from './signaling';
import type { RoomClient } from './room';
import { AuthManager } from './auth';
import { AuthScreen } from './screens/auth';
import { RoomSettingsScreen } from './screens/room-settings';
import { AccountSessionSync } from './account-session-sync';
import { AccountPreferences } from './account-preferences';
import { AccountUpdates, accountUpdatesLive } from './account-updates';
import { AppNavigation, type AppDestination } from './app-navigation';
import { RoomNavigation } from './room-navigation';
import { ClientTelemetry } from './telemetry';
import { CallOutcomeTelemetry, MediaTelemetry } from './media-telemetry';
import { CallHealth, type CallHealthSnapshot } from './call-health';
import { mountCallHealth } from './call-health-ui';
import { AppUpdates } from './app-updates';
import { MediaControls } from './media-controls';
import { MediaLifecycle } from './media-lifecycle';
import { PwaControls } from './pwa';
import { NotificationPreferences } from './notification-preferences';
import { takeContactOffer } from './discovery';
import { openPrivateInbox, openPrivateConversation } from './chat-history';
import { SocialChat } from './social-chat';
import { CommunityUI } from './community-ui';
import { ParticipantHovercard } from './participant-hovercard';
import { api, button, modal, setModalNavigation } from './ui';
import { observeVideoLayout } from './video-layout';
import type { ServerCapabilities } from './api-validation';

// Reserve dependency order before construction; fill each slot immediately when
// its resource is returned. A failed constructor still owns its own partial work.
const CLEANUP_ORDER = [
  'telemetry',
  'auth',
  'signaling',
  'videoLayout',
  'callHealthButton',
  'toasts',
  'callTelemetry',
  'mediaTelemetry',
  'callHealth',
  'appUpdates',
  'appUpdateNotice',
  'accountSync',
  'accountUpdates',
  'accountPreferences',
  'notificationPreferences',
  'pwa',
  'socialChat',
  'community',
  'participantHovercard',
  'mediaLifecycle',
  'mediaControls',
  'inboxRefresh',
  'modalNavigation',
  'navigation',
  'directoryScreen',
  'createRoomScreen',
  'home',
  'authScreen',
  'invitations',
  'chrome',
  'roomLayout',
  'roomSettingsScreen',
  'appSettings',
  'callHealthView',
  'roomController',
  'departure',
] as const;
type AppCleanups = Partial<Record<(typeof CLEANUP_ORDER)[number], () => void>>;

/** Mount one app on a validated static shell; dispose retires its views and starts room departure.
 * A persisted pagehide keeps the instance alive for back/forward-cache restoration.
 * Failed composition retires every resource already returned by a constructor. */
export function startApp(elements: Elements, revision: string): Disposable {
  const scope = createScope();
  const cleanups: AppCleanups = {};
  for (const name of CLEANUP_ORDER) scope.onDispose(() => cleanups[name]?.());
  try {
    initializeApp(elements, revision, scope, cleanups);
    return scope;
  } catch (error) {
    try {
      scope.dispose();
    } catch (cleanupError) {
      throw Object.assign(new Error('App initialization and cleanup failed.'), {
        cause: error,
        cleanupError,
      });
    }
    throw error;
  }
}

function initializeApp(
  elements: Elements,
  revision: string,
  scope: Scope,
  cleanups: AppCleanups,
): void {
  const telemetry = new ClientTelemetry(revision);
  cleanups.telemetry = () => telemetry.dispose();
  window.addEventListener('error', () => telemetry.record({ name: 'js_error', outcome: 'error' }), {
    signal: scope.signal,
  });
  window.addEventListener(
    'unhandledrejection',
    () => telemetry.record({ name: 'unhandled_rejection', outcome: 'error' }),
    { signal: scope.signal },
  );

  const {
    connectionStatus,
    nameInput,
    roomInput,
    joinBtn,
    videoGrid,
    qualityIndicator,
    logoutBtn,
    toastContainer,
    roomVolumeSlot,
  } = elements;
  const stopVideoLayout = observeVideoLayout(videoGrid);
  cleanups.videoLayout = stopVideoLayout;
  const callHealthButton = button('Call health', openCallHealth, 'call-health-button');
  cleanups.callHealthButton = () => {
    callHealthButton.before(qualityIndicator);
    callHealthButton.remove();
  };
  callHealthButton.id = 'call-health-btn';
  callHealthButton.hidden = true;
  qualityIndicator.before(callHealthButton);
  callHealthButton.prepend(qualityIndicator);
  qualityIndicator.setAttribute('aria-hidden', 'true');

  const toasts = createToasts(document, elements.toastContainer);
  cleanups.toasts = () => toasts.dispose();
  const { showToast, showActionToast } = toasts;
  const { observeUiTask, asyncUiAction } = createUiTasks({
    owner: () => ({ room, accountId: auth.userId }),
    notify: showToast,
    signal: scope.signal,
  });
  const chrome = mountChrome(elements, {
    getRoom: () => room,
    get settings() {
      return settingsSheet;
    },
    get telemetry() {
      return telemetry;
    },
    get layout() {
      return roomLayout;
    },
    notify: showToast,
    observe: observeUiTask,
    asyncAction: asyncUiAction,
  });
  cleanups.chrome = () => chrome.dispose();
  const {
    diagnosticsButton,
    showKeyboardShortcuts,
    showRoomExitNotice,
    setRoomToolsVisible,
    placeDiagnosticsButton,
    announceRoomEntry,
    setButtonContent,
  } = chrome;

  // --- Auth ---
  let capabilities: ServerCapabilities | null = null;
  const auth = new AuthManager();
  cleanups.auth = () => auth.dispose();
  auth.setTelemetryHandler(telemetry.record);
  const authScreen = new AuthScreen({
    elements,
    auth,
    capabilities: () => capabilities,
    telemetry,
    asyncAction: asyncUiAction,
    notify: showToast,
    spendRegistrationInvite: (code) => {
      invitations.spendRegistrationInvite(code);
    },
  });
  cleanups.authScreen = () => authScreen.dispose();

  const home = createHomeScreen(
    elements,
    {
      get capabilities() {
        return capabilities;
      },
      set capabilities(value) {
        capabilities = value;
      },
      get restoredAccount() {
        return restoredAccount;
      },
      set restoredAccount(value) {
        restoredAccount = value;
      },
      get room() {
        return room;
      },
      set room(value) {
        room = value;
      },
      get navigationPending() {
        return navigationPending;
      },
      set navigationPending(value) {
        navigationPending = value;
      },
      get departureInProgress() {
        return departureInProgress;
      },
      set departureInProgress(value) {
        departureInProgress = value;
      },
      get roomSelectionVersion() {
        return roomSelectionVersion;
      },
      set roomSelectionVersion(value) {
        roomSelectionVersion = value;
      },
    },
    {
      get auth() {
        return auth;
      },
      get signaling() {
        return signaling;
      },
      get directoryScreen() {
        return directoryScreen;
      },
      get community() {
        return community;
      },
      get pwa() {
        return pwa;
      },
      get accountUpdates() {
        return accountUpdates;
      },
      get accountPreferences() {
        return accountPreferences;
      },
      get notificationPreferences() {
        return notificationPreferences;
      },
      get appNavigation() {
        return appNavigation;
      },
      get participantHovercard() {
        return participantHovercard;
      },
      get authScreen() {
        return authScreen;
      },
      get navigation() {
        return navigation;
      },
      loadCapabilities: () => api.capabilities(),
      previewPendingInvite: () => invitations.previewPendingInvite(),
      reviewPendingContact: () => invitations.reviewPendingContact(),
      hasPendingInvite: () => invitations.hasPending,
      observeUiTask,
      asyncUiAction,
    },
  );
  cleanups.home = () => home.dispose();
  const { updateAuthUI, updateJoinBtn } = home;

  let restoredAccount = false;
  const invitations = createInvitations(
    {
      get capabilities() {
        return capabilities;
      },
      get restoredAccount() {
        return restoredAccount;
      },
      get room() {
        return room;
      },
    },
    {
      get auth() {
        return auth;
      },
      get navigation() {
        return navigation;
      },
      get appNavigation() {
        return appNavigation;
      },
      get community() {
        return community;
      },
      get authScreen() {
        return authScreen;
      },
      takeContactOffer,
      notify: showToast,
      observe: observeUiTask,
    },
  );
  cleanups.invitations = () => invitations.dispose();
  /** An account-room action keeps its explicit join intent until signaling is ready. */
  function openRoomFromDialog(id: string): void {
    if (!auth.isLoggedIn || room?.currentRoomId === id) return;
    if (auth.displayName && !nameInput.value.trim()) nameInput.value = auth.displayName;
    navigation.requestJoin(id);
  }
  const enterRoomFromDirectory = createDirectoryEntry({
    nameInput,
    getDisplayName: () => auth.displayName,
    requestJoin: (id) => navigation.requestJoin(id),
    selectRoom: (id) => navigation.selectRoom(id),
    updateJoin: () => updateJoinBtn(),
  });
  invitations.readInviteLink();

  auth.setOnChange((loggedIn, tokenRefresh) => {
    if (scope.signal.aborted) return;
    if (loggedIn) restoredAccount = true;
    if (!tokenRefresh) notificationPreferences.reset();
    updateAuthUI();
    if (loggedIn && tokenRefresh) {
      // Renew the existing socket as well as the next handshake, preserving room
      // membership and media across the original token's expiry.
      signaling.setToken(auth.jwt ?? undefined);
      return;
    }
    // Identity changes leave the old membership before reconnecting.
    invitations.accountChanged();
    navigation.cancelPendingJoin();
    createRoomScreen.resetIdentity();
    if (room) observeUiTask(roomController.leaveCurrentRoom(), 'Could not finish leaving the room');
    signaling.disconnect();
    signaling.connect(loggedIn ? (auth.jwt ?? undefined) : undefined);
    if (loggedIn) observeUiTask(invitations.previewPendingInvite(), 'Invitation not reviewed');
    if (loggedIn) invitations.reviewPendingContact();
  });

  const localPreferences = createLocalPreferences(() => localStorage);
  const { readLocalPreference, writeLocalPreference } = localPreferences;

  // --- State ---
  let room: RoomClient | null = null;
  cleanups.departure = () => {
    const departing = room;
    room = null;
    departing?.leave().catch(() => console.error('Could not finish leaving the room'));
  };
  let selectedRoomId = '';
  const mediaControls = new MediaControls({
    getRoom: () => room,
    notify: (message) => showToast(message),
    onPlaybackResult: (element, blocked) => {
      callTelemetry.playbackResult(element, blocked);
      callHealth.playbackResult(element, blocked);
    },
    talkRow: () => roomController.buildTalkRow(),
  });
  cleanups.mediaControls = () => mediaControls.destroy();
  mediaControls.mountToolbar(roomVolumeSlot);

  const roomLayout = createRoomLayout(elements, {
    readPreference: readLocalPreference,
    writePreference: writeLocalPreference,
    renderParticipants: () => {
      if (room) roomController.renderParticipants(room.getParticipants());
    },
  });
  cleanups.roomLayout = () => roomLayout.dispose();

  const roomSettingsScreen = new RoomSettingsScreen({
    store: { getRoom: () => room },
    notify: showToast,
    asyncAction: asyncUiAction,
  });
  cleanups.roomSettingsScreen = () => roomSettingsScreen.dispose();

  const appSettings = createAppSettings(
    elements,
    {
      get room() {
        return room;
      },
    },
    {
      get auth() {
        return auth;
      },
      get pwa() {
        return pwa;
      },
      get accountPreferences() {
        return accountPreferences;
      },
      get community() {
        return community;
      },
      get navigation() {
        return appNavigation;
      },
      get mediaControls() {
        return mediaControls;
      },
      get socialChat() {
        return socialChat;
      },
      get roomSettings() {
        return roomSettingsScreen;
      },
      get layout() {
        return roomLayout;
      },
      get diagnosticsButton() {
        return diagnosticsButton;
      },
      showKeyboardShortcuts,
    },
  );
  cleanups.appSettings = () => appSettings.dispose();
  const settingsSheet = appSettings.sheet;
  let navigationPending = false;
  let roomSelectionVersion = 0;
  let departureInProgress: Promise<void> | null = null;
  const savedName = readLocalPreference('displayName');
  if (savedName) nameInput.value = savedName;

  // --- Signaling setup ---
  const wsProtocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
  const wsUrl = `${wsProtocol}//${window.location.host}/ws`;
  const signaling = new SignalingClient(wsUrl, async (token, signal) => {
    const reply = await api.websocketTicket(token, signal);
    return { ticket: reply.ticket, expiresIn: reply.expires_in };
  });
  cleanups.signaling = () => signaling.disconnect();
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
      for (const element of roomController.mediaElements()) {
        if (!(element.srcObject instanceof MediaStream)) continue;
        for (const track of element.srcObject.getTracks()) {
          if (!elements.has(track)) elements.set(track, element);
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
  cleanups.callTelemetry = () => callTelemetry.dispose();
  const mediaTelemetry = new MediaTelemetry(
    () => (telemetry.sharingEnabled ? (room?.telemetrySources() ?? []) : []),
    telemetry.record,
    callTelemetry,
    telemetry.recordMediaSample,
  );
  cleanups.mediaTelemetry = () => mediaTelemetry.dispose();
  let callHealthView: ReturnType<typeof modal> | null = null;
  cleanups.callHealthView = () => callHealthView?.close();
  let updateCallHealthView: ((snapshot: CallHealthSnapshot) => void) | null = null;
  const callHealth = new CallHealth({
    context: () =>
      room
        ? {
            scope: room,
            membership: room.membershipVersion,
            state: room.callHealthState(),
          }
        : null,
    sources: () => room?.callHealthSources() ?? [],
    playback: () => roomController.playbackElements(),
    changed: renderCallHealth,
  });
  cleanups.callHealth = () => callHealth.dispose();
  const appUpdateNotice = createAppUpdateNotice(toastContainer, () => appUpdates.reloadWhenIdle());
  cleanups.appUpdateNotice = () => appUpdateNotice.dispose();
  const appUpdates = new AppUpdates({
    revision,
    inCall: () => room !== null || departureInProgress !== null || navigationPending,
    available: appUpdateNotice.renderAppUpdateAvailable,
    reload: () => location.reload(),
  });
  cleanups.appUpdates = () => appUpdates.dispose();
  const notificationPreferences = new NotificationPreferences({
    getToken: () => auth.jwt,
    getAccountId: () => auth.userId,
    notify: (message) => showToast(message),
  });
  cleanups.notificationPreferences = () => notificationPreferences.dispose();
  const mountConversationNotifications = (
    container: HTMLElement,
    peerId: string,
    current: () => boolean,
  ): void => {
    notificationPreferences.mountConversation(container, peerId, current);
  };
  const accountUpdates = new AccountUpdates({
    getToken: () => auth.jwt,
    getAccountId: () => auth.userId,
  });
  cleanups.accountUpdates = () => accountUpdates.dispose();
  const accountPreferences = new AccountPreferences({
    getToken: () => auth.jwt,
    getAccountId: () => auth.userId,
    notify: (message) => showToast(message),
  });
  cleanups.accountPreferences = () => accountPreferences.dispose();
  const mountConversationPrivacy = (
    container: HTMLElement,
    peerId: string,
    peerName: string,
    current: () => boolean,
  ): void => {
    accountPreferences.mountConversation(container, peerId, peerName, current);
  };
  function openAccountConversation(peerId: string, peerName: string): void {
    openPrivateConversation(peerId, peerName, {
      getToken: () => auth.jwt,
      getAccountId: () => auth.userId,
      getRoomId: () => room?.currentRoomId ?? null,
      mountConversationNotifications,
      mountConversationPrivacy,
    });
  }
  function openAccountMessages(): void {
    if (!auth.isLoggedIn) {
      authScreen.open('login');
      return;
    }
    openPrivateInbox({
      getToken: () => auth.jwt,
      getAccountId: () => auth.userId,
      getRoomId: () => room?.currentRoomId ?? null,
      mountConversationNotifications,
      mountConversationPrivacy,
    });
  }
  function restoreAppDestination(destination: AppDestination): void {
    if (destination.kind === 'inbox') openAccountMessages();
    else if (destination.kind === 'conversation')
      openAccountConversation(destination.peerId, destination.peerName ?? 'Contact');
    else
      settingsSheet.open({
        ...(destination.section && { section: destination.section }),
        ...(destination.segment && { segment: destination.segment }),
      });
  }
  const socialChat = new SocialChat({
    elements,
    accountPreferences,
    openAccountConversation,
    telemetry: telemetry.record,
    getRoom: () => room,
    getViewerKey: () => auth.userId ?? 'guest',
    getToken: () => auth.jwt,
    notify: (message) => showToast(message),
    bindParticipantName: (anchor, id, name) => participantHovercard.bind(anchor, id, name),
    allowsNotification: (kind, peer) => notificationPreferences.allows(kind, peer),
    mountConversationNotifications,
    openSettings: (section, opener) => settingsSheet.open({ section, opener }),
    mountAccountNotifications: (container, current) => pwa.mountAccount(container, current),
  });
  cleanups.socialChat = () => socialChat.dispose();
  const pwa = new PwaControls({
    getToken: () => auth.jwt,
    getAccountId: () => auth.userId,
    mountNotificationPreferences: (container, current) =>
      notificationPreferences.mountAccount(container, current),
    openMessages: openAccountMessages,
  });
  cleanups.pwa = () => pwa.dispose();
  const community = new CommunityUI({
    elements,
    onOpenMessages: openAccountMessages,
    openSettings: (section, opener) => settingsSheet.open({ section, opener }),
    onAccountChanged: () => settingsSheet.close({ force: true }),
    mountConversationNotifications,
    mountConversationPrivacy,
    auth,
    getRoom: () => room,
    notify: (message) => showToast(message),
    onProfileChanged: (profile) => {
      participantHovercard.reset();
      auth.updateDisplayName(profile.display_name);
      if (room) roomController.renderParticipants(room.getParticipants());
    },
    onRoomsChanged: () =>
      observeUiTask(directoryScreen.load(), 'Could not refresh the room directory'),
    onForgetDevice: () => observeUiTask(signOutAndForget(), 'Could not sign out'),
    onJoinRoom: openRoomFromDialog,
    onRoomDeleted: async (id) => {
      if (room?.currentRoomId === id) await roomController.leaveCurrentRoom();
    },
    onSignedOut: async () => {
      await roomController.leaveCurrentRoom();
      auth.forgetSession();
    },
  });
  cleanups.community = () => community.dispose();

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
        ...(role && { role: roomController.roleName(role) }),
        ...(color && { color }),
        profileAvailable: self ? auth.isLoggedIn : person?.authenticated === true,
        canMessage: online && !self && room.canChat,
        canContact: !self && auth.isLoggedIn && person?.authenticated === true,
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
    onContact: (id) => community.requestContact(id),
    onProfile: (id) => observeUiTask(community.showProfile(id), 'Could not open the profile'),
    onMore: (id, name, anchor) => {
      const bounds = anchor.getBoundingClientRect();
      roomController.showModerationMenu(id, name, bounds.left, bounds.bottom);
    },
  });
  cleanups.participantHovercard = () => participantHovercard.destroy();

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
  cleanups.mediaLifecycle = () => mediaLifecycle.dispose();
  const refreshSavedMessages = () => {
    if (document.visibilityState === 'hidden') return;
    if (!accountUpdatesLive(auth.userId) || community.messagesNeedRefresh)
      community.refreshMessages().catch(() => {});
    socialChat.refreshSavedChat();
  };
  const inboxRefresh = setInterval(refreshSavedMessages, 30_000);
  cleanups.inboxRefresh = () => clearInterval(inboxRefresh);
  document.addEventListener('visibilitychange', refreshSavedMessages, { signal: scope.signal });
  signaling.setOnStatusChange((status) => {
    if (scope.signal.aborted) return;
    const awaitingRoom = status === 'connected' && roomController.recovering;
    connectionStatus.textContent = awaitingRoom
      ? 'Rejoining room…'
      : status.charAt(0).toUpperCase() + status.slice(1);
    connectionStatus.className = `status ${awaitingRoom ? 'connecting' : status}`;
    updateJoinBtn();
    document.getElementById('connection-retry-notice')?.remove();
    if (status === 'connected' && !room?.currentRoomId) signaling.completeRestartRecovery();
    // Let the socket finish notifying its existing recovery owner before a new
    // room registers callbacks. The navigation rechecks ownership and readiness.
    if (status === 'connected')
      queueMicrotask(() => {
        if (!scope.signal.aborted) navigation.resumePendingJoin();
      });
    if (status === 'disconnected' && signaling.reconnectExhausted && !room?.currentRoomId) {
      const notice = showActionToast(
        'The server is still unavailable. Retry when ready.',
        [{ label: 'Retry connection', action: () => signaling.retryConnection() }],
        0,
      );
      notice.id = 'connection-retry-notice';
    }
  });

  // Home screens read current identity and selection through these getters.
  const directoryScreen = new DirectoryScreen(
    elements,
    {
      get capabilities() {
        return capabilities;
      },
      get accountId() {
        return auth.userId;
      },
      get token() {
        return auth.jwt;
      },
      get isLoggedIn() {
        return auth.isLoggedIn;
      },
      get selectedRoomId() {
        return roomInput.value.trim();
      },
    },
    {
      enterRoom: enterRoomFromDirectory,
      observe: observeUiTask,
      notify: (message) => showToast(message),
      readPreference: readLocalPreference,
      writePreference: writeLocalPreference,
      tone: roomTone,
    },
  );
  cleanups.directoryScreen = () => directoryScreen.dispose();
  const createRoomScreen = new CreateRoomScreen({
    elements,
    store: {
      get capabilities() {
        return capabilities;
      },
      get accountId() {
        return auth.userId;
      },
      get token() {
        return auth.jwt;
      },
      get isLoggedIn() {
        return auth.isLoggedIn;
      },
      get selectionVersion() {
        return roomSelectionVersion;
      },
    },
    notify: showToast,
    asyncAction: asyncUiAction,
    created: (id) => {
      roomInput.value = id;
      if (auth.displayName && !nameInput.value.trim()) nameInput.value = auth.displayName;
      updateJoinBtn();
      joinBtn.click();
    },
  });
  cleanups.createRoomScreen = () => createRoomScreen.dispose();

  const appNavigation = new AppNavigation({
    getRoom: () => room?.currentRoomId ?? selectedRoomId,
    getAccountId: () => auth.userId,
    ready: () => capabilities !== null && restoredAccount,
    restore: restoreAppDestination,
    error: (message) => showToast(message, 4000, 'error'),
  });
  const stopModalNavigation = setModalNavigation((dialog, canDismiss, route) =>
    appNavigation.opened({
      owner: dialog,
      ...(route && { destination: route.destination, accountId: route.accountId }),
      canLeave: canDismiss,
      close: () => {
        if (canDismiss()) dialog.close();
      },
    }),
  );
  cleanups.modalNavigation = stopModalNavigation;
  const roomController = mountRoomScreen(
    elements,
    {
      get room() {
        return room;
      },
      set room(value) {
        room = value;
      },
      get selectedRoomId() {
        return selectedRoomId;
      },
      set selectedRoomId(value) {
        selectedRoomId = value;
      },
      get navigationPending() {
        return navigationPending;
      },
      set navigationPending(value) {
        navigationPending = value;
      },
      get roomSelectionVersion() {
        return roomSelectionVersion;
      },
      set roomSelectionVersion(value) {
        roomSelectionVersion = value;
      },
      get departureInProgress() {
        return departureInProgress;
      },
      set departureInProgress(value) {
        departureInProgress = value;
      },
    },
    {
      get auth() {
        return auth;
      },
      get signaling() {
        return signaling;
      },
      get socialChat() {
        return socialChat;
      },
      get community() {
        return community;
      },
      get participantHovercard() {
        return participantHovercard;
      },
      get mediaControls() {
        return mediaControls;
      },
      get settingsSheet() {
        return settingsSheet;
      },
      get roomSettingsScreen() {
        return roomSettingsScreen;
      },
      get roomLayout() {
        return roomLayout;
      },
      get callTelemetry() {
        return callTelemetry;
      },
      get callHealth() {
        return callHealth;
      },
      get mediaTelemetry() {
        return mediaTelemetry;
      },
      get telemetry() {
        return telemetry;
      },
      get navigation() {
        return navigation;
      },
      showToast,
      observeUiTask,
      asyncUiAction,
      showActionToast,
      showRoomExitNotice,
      setRoomToolsVisible,
      placeDiagnosticsButton,
      updateJoinBtn,
      readLocalPreference,
      writeLocalPreference,
      announceRoomEntry,
      setButtonContent,
      refreshDirectory: () => directoryScreen.load(),
      setCallHealthVisible: (visible) => {
        callHealthButton.hidden = !visible;
      },
      closeCallHealth: () => {
        callHealthView?.close();
      },
      dismissHomeDialogs: () => {
        authScreen.dismiss();
        createRoomScreen.dismiss();
      },
    },
  );
  cleanups.roomController = () => roomController.dispose();

  const navigation = new RoomNavigation({
    readRoom: () => appNavigation.readRoom(),
    interceptLocation: (selectedRoom) =>
      invitations.consumeInviteLocation(selectedRoom) ||
      appNavigation.locationChanged(selectedRoom),
    leave: () => roomController.leaveCurrentRoom(),
    select: (id) => {
      selectedRoomId = id;
      roomSelectionVersion++;
      invitations.selectedRoomChanged();
      roomInput.value = id;
      appNavigation.synchronize();
      updateJoinBtn();
      directoryScreen.select(id);
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
  cleanups.navigation = () => navigation.dispose();

  const accountSync = new AccountSessionSync({
    reconcile: () => auth.reconcileSharedSession(),
    invalidate: () => {
      authScreen.retire();
      auth.invalidateSharedSession();
    },
    canCheck: () => auth.canCheckSharedSession,
  });
  cleanups.accountSync = () => accountSync.dispose();
  auth.setSessionMutationHandler(() => accountSync.publish());

  home.start();

  function signOutAndForget(): Promise<void> {
    return forgetSignIn({
      auth,
      nameInput,
      accountEpoch: () => invitations.accountEpoch,
      clearPreferences: () => localPreferences.clear(),
      forgetDevice: () => forgetThisDevice(localStorage),
      reload: () => {
        if (!scope.signal.aborted) location.reload();
      },
    });
  }

  logoutBtn.addEventListener(
    'click',
    () => {
      signOutAndForget().catch((error) => {
        if (!scope.signal.aborted)
          showToast(error instanceof Error ? error.message : 'Sign out failed');
      });
    },
    { signal: scope.signal },
  );
  function renderCallHealth(snapshot: CallHealthSnapshot): void {
    const quality = {
      good: 'good',
      warning: 'fair',
      problem: 'poor',
      unknown: 'unknown',
      idle: 'unknown',
    }[snapshot.level];
    qualityIndicator.className = `quality-dot quality-${quality}`;
    qualityIndicator.title = 'Current local media evidence';
    callHealthButton.title = `Sending: ${snapshot.sending.label}. Receiving: ${snapshot.receiving.label}. Playback: ${snapshot.playback.label}.`;
    updateCallHealthView?.(snapshot);
  }

  function openCallHealth(): void {
    if (!room || callHealthView?.dialog.open) return;
    const owner = room;
    const membership = owner.membershipVersion;
    const view = modal('Call health');
    callHealthView = view;
    const current = (): boolean => room === owner && owner.membershipVersion === membership;
    updateCallHealthView = mountCallHealth(view.body, {
      retryPlayback: () => {
        if (!current()) return;
        mediaControls.resumePlayback();
        callHealth.sample();
      },
      speaker: () => {
        if (!current()) return;
        view.close();
        settingsSheet.open({ section: 'speaker', opener: callHealthButton });
      },
      capture: () => {
        if (!current()) return;
        view.close();
        settingsSheet.open({ section: 'camera', opener: callHealthButton });
      },
      refreshIncoming: () => {
        if (current() && roomController.canRefreshIncomingMedia)
          roomController.refreshIncomingMedia();
      },
      canRefresh: () =>
        current() &&
        owner.connected &&
        !owner.callHealthState().receiveRecovering &&
        roomController.canRefreshIncomingMedia,
    });
    updateCallHealthView(callHealth.snapshot());
    view.dialog.addEventListener(
      'close',
      () => {
        if (callHealthView !== view) return;
        callHealthView = null;
        updateCallHealthView = null;
      },
      { once: true },
    );
  }

  window.addEventListener(
    'pagehide',
    (event) => {
      if (!event.persisted) scope.dispose();
    },
    { signal: scope.signal },
  );
}
