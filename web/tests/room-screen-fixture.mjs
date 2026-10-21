import { loadTypeScript } from './native-fixtures.mjs';
import { uiFixture, flush, deferred } from './ui-fixture.mjs';

/** Real room factories with an owned text-only DOM and injected network/media boundaries. */
export async function roomScreenFixture({
  room: initialRoom = null,
  mode = 'open',
  globals = {},
} = {}) {
  const f = await uiFixture();
  const { Node, document } = f;
  const originalAdd = Node.prototype.addEventListener;
  Node.prototype.removeEventListener = function (type, callback) {
    this.listeners.set(
      type,
      (this.listeners.get(type) ?? []).filter((entry) => entry.callback !== callback),
    );
  };
  Node.prototype.addEventListener = function (type, callback, options = {}) {
    if (options.signal?.aborted) return;
    originalAdd.call(this, type, callback, options);
    options.signal?.addEventListener('abort', () => this.removeEventListener(type, callback), {
      once: true,
    });
  };
  Node.prototype.focus = function () {
    document.activeElement = this;
  };
  Node.prototype.insertAdjacentHTML = function () {};
  Node.prototype.prepend = function (...nodes) {
    for (const node of nodes.reverse()) this.insertBefore(node, this.firstChild);
  };
  Node.prototype.contains = function (node) {
    for (let current = node; current; current = current.parentNode)
      if (current === this) return true;
    return false;
  };
  Node.prototype.closest = function () {
    return this.interactive ? this : null;
  };
  Object.defineProperties(Node.prototype, {
    dataset: {
      get() {
        this._dataset ??= {};
        return this._dataset;
      },
    },
    style: {
      get() {
        this._style ??= {
          setProperty(name, value) {
            this[name] = value;
          },
        };
        return this._style;
      },
    },
    classList: {
      get() {
        const node = this;
        const names = () => (node.className ?? '').split(/\s+/).filter(Boolean);
        return {
          add(...values) {
            node.className = [...new Set([...names(), ...values])].join(' ');
          },
          remove(...values) {
            node.className = names()
              .filter((n) => !values.includes(n))
              .join(' ');
          },
          contains(value) {
            return names().includes(value);
          },
          toggle(value, force) {
            const next = force ?? !names().includes(value);
            node.className = (
              next ? [...new Set([...names(), value])] : names().filter((n) => n !== value)
            ).join(' ');
            return next;
          },
        };
      },
    },
  });
  document.activeElement = document.body;
  document.addEventListener = document.body.addEventListener.bind(document.body);
  document.removeEventListener = document.body.removeEventListener.bind(document.body);
  document.createTextNode = (value) => {
    const node = new Node('text');
    node.textContent = value;
    return node;
  };
  const window = new Node('window');
  window.innerWidth = 1440;
  window.innerHeight = 900;
  const elements = {};
  for (const name of [
    'handBtn',
    'roomSettingsBtn',
    'chatInput',
    'chatSendBtn',
    'screenBtn',
    'camBtn',
    'lobbyTab',
    'lobbyListEl',
    'lobbyEmpty',
    'joinBtn',
    'nameInput',
    'roomInput',
    'participantList',
    'roomLabel',
    'roomTopic',
    'joinScreen',
    'roomScreen',
    'lobbyScreen',
    'lobbyRoomName',
    'lobbyTopic',
    'connectionStatus',
    'micBtn',
    'qualityIndicator',
    'lobbyCount',
    'lobbyCancelBtn',
    'videoGrid',
    'chatMessages',
    'unreadBadge',
    'scrollBottomBtn',
    'leaveBtn',
    'homeLink',
    'settingsBtn',
    'usersPanel',
    'roomMoreRepair',
    'roomNavigation',
  ]) {
    elements[name] = new Node(
      name.endsWith('Btn') ? 'button' : name.endsWith('Input') ? 'input' : 'div',
    );
    document.body.append(elements[name]);
  }
  elements.nameInput.value = 'Fixture person';
  elements.roomInput.value = 'fixture-room';
  elements.roomScreen.hidden = false;
  elements.usersPanel.classList.add('active');
  const store = {
    room: initialRoom,
    selectedRoomId: '',
    navigationPending: false,
    roomSelectionVersion: 0,
    departureInProgress: null,
  };
  const state = {
    events: null,
    rooms: [],
    toasts: [],
    notices: [],
    system: [],
    buttonUpdates: [],
    preferences: [],
    calls: [],
    errors: [],
    tasks: [],
    capture: [],
    roomDefaults: {},
  };
  class RoomPasswordRequiredError extends Error {}
  const roomDefaults = {
    role: 'member',
    membershipVersion: 1,
    hasMedia: false,
    audioEnabled: false,
    videoEnabled: false,
    isScreenSharing: false,
    localParticipantId: null,
    currentRoomId: 'fixture-room',
    nickname: 'Fixture person',
    connected: true,
    roomSettings: {
      displayName: 'Fixture room',
      allowChat: true,
      allowVideo: true,
      allowScreenSharing: true,
      guestsAllowed: true,
      guestsCanBroadcast: true,
      moderated: false,
      pushToTalk: false,
    },
    getParticipants: () => new Map(),
    getLocalStream: () => null,
    preferChatStyle() {},
    join: async () => 'lobby',
    leave: async () => {},
    retryRecovery() {},
    muteAudio() {
      this.audioEnabled = false;
    },
    setPinnedRemoteVideo() {},
    setPictureInPictureVideo() {},
    setRemoteVideoVisibility() {},
    setRemoteVideoSizeCap() {},
  };
  class RoomClient {
    constructor(_signaling, events) {
      Object.assign(this, roomDefaults, state.roomDefaults);
      state.events = events;
      state.rooms.push(this);
    }
  }
  const makeRoom = (overrides) => Object.assign({}, roomDefaults, overrides);
  if (initialRoom) store.room = makeRoom(initialRoom);
  const observe = (task, message) => {
    state.tasks.push(
      Promise.resolve(task).catch((error) => {
        state.errors.push({ error, message });
      }),
    );
  };
  const services = {
    auth: { isLoggedIn: false, userId: null },
    signaling: { connected: true, reconnectExhausted: false, retryConnection() {} },
    socialChat: {
      system: (text) => state.system.push(text),
      savedLook: () => ({ color: null, style: 'accent' }),
      activate: async () => {},
      reset() {},
      participantsChanged() {},
      prepareSnapshotRecovery: () => state.calls.push('capture'),
      handleEvent() {},
      refreshSavedChat() {},
    },
    community: {
      refresh() {},
      retainProfiles() {},
      decorateAvatar() {},
      showProfile: async () => {},
      report() {},
      ban() {},
    },
    participantHovercard: { close() {}, reset() {}, refresh() {}, bind() {} },
    mediaControls: {
      reset() {},
      detachParticipant: (id) => state.calls.push(['detach', id]),
      detachTile() {},
      attachTile() {},
    },
    settingsSheet: { open() {}, close() {}, refresh() {} },
    roomSettingsScreen: {
      pending: false,
      isOpen: false,
      retire: () => state.calls.push('retire'),
      sync() {},
    },
    roomLayout: {
      layout: 'modern',
      desktop: true,
      preferences: { rosterCollapsed: false, chatCollapsed: false },
      mobileCollapsed: false,
      apply() {},
      setLayout() {},
      selectTab() {},
      attachPanelResize() {},
    },
    callTelemetry: { signal() {} },
    callHealth: { sample() {} },
    mediaTelemetry: {},
    telemetry: { record() {} },
    navigation: { join: () => true, home() {}, resumePendingJoin() {} },
    showToast: (...args) => state.toasts.push(args),
    observeUiTask: observe,
    asyncUiAction: (action, message) => () => {
      try {
        observe(action(), message);
      } catch (error) {
        observe(Promise.reject(error), message);
      }
    },
    showActionToast(message, actions, duration) {
      const node = new Node('div');
      document.body.append(node);
      state.notices.push({ message, actions, duration, node });
      return node;
    },
    showRoomExitNotice: (title, message) => state.notices.push({ title, message }),
    setRoomToolsVisible() {},
    placeDiagnosticsButton() {},
    updateJoinBtn() {},
    readLocalPreference: (key) => (key === 'micMode' ? mode : null),
    writeLocalPreference: (...args) => state.preferences.push(args),
    announceRoomEntry() {},
    setButtonContent: (button, _icon, tooltip) => state.buttonUpdates.push({ button, tooltip }),
    refreshDirectory: async () => {},
    setCallHealthVisible() {},
    closeCallHealth() {},
    dismissHomeDialogs() {},
  };
  const scopeModule = await loadTypeScript('src/disposable.ts');
  const colors = await loadTypeScript('src/avatar-colors.ts');
  const icons = await loadTypeScript('src/icons.ts');
  const sheet = await loadTypeScript('src/settings-sheet.ts', {
    modules: { './ui': f.ui, './settings-sheet.css': {} },
    globals: { document },
  });
  const mediaControls = await loadTypeScript('src/media-controls.ts', {
    modules: {
      './receive-policy': await loadTypeScript('src/receive-policy.ts'),
      './capture-preferences': await loadTypeScript('src/capture-preferences.ts'),
      './media-controls.css': {},
      './settings-sheet': sheet,
      './audio-output': await loadTypeScript('src/audio-output.ts'),
    },
  });
  const mediaStreams = class {
    constructor(tracks = []) {
      this.tracks = tracks;
    }
    getTracks() {
      return this.tracks;
    }
  };
  const moduleGlobals = {
    document,
    window,
    Element: Node,
    navigator: { mediaDevices: { getDisplayMedia() {} } },
    MediaStream: mediaStreams,
    CSS: { escape: (value) => value },
    console: { error() {}, warn() {} },
    ...globals,
  };
  const mediaModule = await loadTypeScript('src/screens/room-media.ts', {
    modules: {
      '../media-controls': mediaControls,
      '../media-telemetry': { observeFirstVideoFrame() {} },
      '../layer-cap': await loadTypeScript('src/layer-cap.ts'),
      '../avatar-colors': colors,
      '../icons': icons,
      '../ui': f.ui,
      '../disposable': scopeModule,
    },
    globals: moduleGlobals,
  });
  const roomModule = await loadTypeScript('src/screens/room.ts', {
    modules: {
      '../room': { RoomClient, RoomPasswordRequiredError },
      '../icons': icons,
      '../disposable': scopeModule,
      '../appearance': { applyAppearance() {} },
      '../avatar-colors': colors,
      '../ui': f.ui,
      '../settings-sheet': sheet,
      './room-media': {
        createRoomMedia(...args) {
          state.publishing = args[3];
          return mediaModule.createRoomMedia(...args);
        },
      },
    },
    globals: moduleGlobals,
  });
  const controller = roomModule.createRoomScreen(elements, store, services);
  return {
    ...f,
    store,
    services,
    state,
    elements,
    controller,
    controls: controller.controls,
    window,
    makeRoom,
    RoomPasswordRequiredError,
    async connect(overrides = {}) {
      state.roomDefaults = overrides;
      store.room = null;
      elements.joinBtn.click();
      await flush();
      return store.room;
    },
    async settled() {
      await Promise.all(state.tasks);
    },
    key(type, key, repeat = false) {
      document.body.emit(type, {
        key,
        repeat,
        target: new Node('div'),
        preventDefault() {
          this.defaultPrevented = true;
        },
      });
    },
    deferred,
  };
}
