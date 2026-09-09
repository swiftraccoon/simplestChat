import type { RoomSettings, ServerMessage, SocialAction, RoomSnapshot } from './protocol';
import { SignalingClient } from './signaling';
import { MediaManager, type CapturePreferences, type RemoteVideoQuality } from './media';

export interface Participant {
  id: string;
  name: string;
  role: string;
  authenticated?: boolean;
  producers: Map<string, { kind: 'audio' | 'video'; source?: string }>;
}

export type ConnectionQuality = 'good' | 'fair' | 'poor' | 'unknown';

export class RoomPasswordRequiredError extends Error {
  constructor() {
    super('This room requires a password');
    this.name = 'RoomPasswordRequiredError';
  }
}

export type RoomEventHandler = {
  onParticipantsChanged: (participants: Map<string, Participant>) => void;
  onLocalStream: (stream: MediaStream) => void;
  onLocalMediaChanged: () => void;
  onRemoteTrack: (participantId: string, participantName: string, track: MediaStreamTrack, kind: 'audio' | 'video', source?: string) => void;
  onRemoteTrackRemoved: (participantId: string, producerId: string, kind: 'audio' | 'video', source?: string) => void;
  onParticipantLeft: (participantId: string) => void;
  onParticipantJoined: (participantId: string, participantName: string) => void;
  onChatMessage: (participantId: string, participantName: string, content: string) => void;
  onConnectionQuality: (quality: ConnectionQuality) => void;
  onActiveSpeaker: (participantId: string) => void;
  onAudioLevels: (levels: { participantId: string; volume: number }[]) => void;
  onModeration: (action: string, participantId: string, reason?: string) => void;
  onRoleChanged: (participantId: string, newRole: string) => void;
  onRoomSettingsChanged: (settings: RoomSettings) => void;
  onTopicChanged: (topic: string, changedBy: string) => void;
  onVoiceRequested: (participantId: string, displayName: string) => void;
  onLobbyWaiting: (roomName: string, topic: string | undefined, count: number) => void;
  onLobbyJoin: (participantId: string, displayName: string) => void;
  onLobbyAdmitted: () => void;
  onLobbyDenied: (reason?: string) => void;
  /** Fired after post-lobby-admission media setup finishes (room state + media are ready) */
  onAdmissionComplete: () => void;
  onSocialEvent?: (message: ServerMessage) => void;
  onRecoveryState?: (state: 'reconnecting' | 'connected' | 'failed', message?: string) => void;
  onPasswordRequired?: () => Promise<string | null>;
  onRoomClosed?: (reason: string) => void;
};

export class RoomClient {
  private signaling: SignalingClient;
  private media: MediaManager | null = null;
  private mediaReady = false;
  private participants = new Map<string, Participant>();
  private localId: string | null = null;
  private roomId: string | null = null;
  private participantName: string | null = null;
  private events: RoomEventHandler;
  private reconnectToken: string | null = null;
  private connectionQuality: ConnectionQuality = 'unknown';
  private localRole: string = 'user';
  private _roomSettings: RoomSettings | null = null;
  private localTextMuted = false;
  private localCamBanned = false;
  private serverCanChat: boolean | null = null;
  private serverCanBroadcast: boolean | null = null;
  private recovering = false;
  private recoveryPromise: Promise<void> | null = null;
  private consumeQueue: Promise<void> = Promise.resolve();
  private pendingConsumes = new Map<string, Promise<void>>();
  // Producers known to be paused — prevents showing black tiles from race condition
  // where ProducerPaused arrives before consumeProducer finishes
  private pausedProducers = new Set<string>();
  private awaitingPostAdmission = false;
  private joinPassword: string | undefined;
  private generation = 0;
  private cancelJoin: (() => void) | null = null;
  private hiddenParticipants = new Set<string>();
  private videoQualities = new Map<string, RemoteVideoQuality>();
  private socialRequests = new Map<string, {
    action: SocialAction;
    resolve: (value: Record<string, unknown>) => void;
    reject: (error: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  }>();

  constructor(signaling: SignalingClient, events: RoomEventHandler) {
    this.signaling = signaling;
    this.events = events;
    this.signaling.setOnMessage((msg) => this.handleMessage(msg));
    this.signaling.setOnReconnected(() => { void this.attemptReconnect(); });
  }

  get localParticipantId(): string | null {
    return this.localId;
  }

  get currentRoomId(): string | null {
    return this.roomId;
  }

  get nickname(): string { return this.participantName ?? ''; }
  get membershipVersion(): number { return this.generation; }
  get connected(): boolean { return this.localId !== null && this.signaling.connected && !this.recovering; }
  get textMuted(): boolean { return this.localTextMuted; }
  get camBanned(): boolean { return this.localCamBanned; }
  get canChat(): boolean {
    if (!this.localId || this.localTextMuted || this._roomSettings?.allowChat === false) return false;
    return this.serverCanChat ?? (!this._roomSettings?.moderated || this.hasVoiceRole());
  }
  get canBroadcast(): boolean {
    if (!this.localId || (this.localRole === 'guest' && this._roomSettings?.guestsCanBroadcast === false)) return false;
    return this.serverCanBroadcast ?? (!this._roomSettings?.moderated || this.hasVoiceRole());
  }

  private hasVoiceRole(): boolean {
    return ['member', 'moderator', 'admin', 'owner'].includes(this.localRole);
  }

  private rejectSocialRequests(message: string): void {
    for (const request of this.socialRequests.values()) {
      clearTimeout(request.timer);
      request.reject(new Error(message));
    }
    this.socialRequests.clear();
  }

  private closeMedia(): void {
    const media = this.media;
    this.media = null;
    this.mediaReady = false;
    this.pendingConsumes.clear();
    this.consumeQueue = Promise.resolve();
    media?.close();
  }

  async join(roomId: string, participantName: string, password?: string): Promise<'joined' | 'lobby'> {
    this.cancelJoin?.();
    const generation = ++this.generation;
    this.recoveryPromise = null;
    this.rejectSocialRequests('Room membership changed');
    this.closeMedia();
    this.participants.clear();
    this.pausedProducers.clear();
    this.awaitingPostAdmission = false;
    this.localId = null;
    this.reconnectToken = null;
    this.localRole = 'user';
    this._roomSettings = null;
    this.localTextMuted = false;
    this.localCamBanned = false;
    this.serverCanChat = null;
    this.serverCanBroadcast = null;
    this.roomId = roomId;
    this.participantName = participantName;
    this.joinPassword = password;

    // Join room — may get roomJoined or lobbyWaiting
    const response = await new Promise<ServerMessage>((resolve, reject) => {
      // Temporarily intercept the next roomJoined or lobbyWaiting message
      const origHandler = this.signaling['onMessage'];
      const cleanup = (): void => {
        clearTimeout(timer);
        if (this.signaling['onMessage'] === interceptor) this.signaling['onMessage'] = origHandler;
        this.cancelJoin = null;
      };
      const timer = setTimeout(() => { cleanup(); reject(new Error('Timeout waiting for join response')); }, 10000);
      const interceptor = (msg: ServerMessage) => {
        if (msg.type === 'roomJoined' || msg.type === 'lobbyWaiting' || msg.type === 'roomPasswordRequired' || msg.type === 'error') {
          cleanup();
          if (msg.type === 'roomPasswordRequired') {
            reject(new RoomPasswordRequiredError());
          } else if (msg.type === 'error') {
            reject(new Error(msg.message));
          } else {
            resolve(msg);
          }
          return;
        }
        origHandler?.(msg);
      };
      this.cancelJoin = () => { cleanup(); reject(new Error('Room join cancelled')); };
      this.signaling['onMessage'] = interceptor;
      try {
        this.signaling.send({ type: 'joinRoom', roomId, participantName, password });
      } catch (error) {
        cleanup();
        reject(error instanceof Error ? error : new Error('Unable to join room'));
      }
    });

    if (generation !== this.generation) throw new Error('Room join cancelled');

    if (response.type === 'lobbyWaiting') {
      this.events.onLobbyWaiting(response.roomName, response.topic, response.participantCount);
      return 'lobby';
    }

    if (response.type !== 'roomJoined') {
      // 'error' is rejected inside the interceptor — this is unreachable,
      // but narrows the type for the code below.
      throw new Error(`Unexpected join response: ${response.type}`);
    }

    this.localId = response.participantId;
    this.reconnectToken = response.reconnectToken;
    this.localRole = response.yourRole ?? 'user';
    this._roomSettings = response.roomSettings ?? null;
    this.recovering = false;

    // Store existing participants
    for (const p of response.participants) {
      this.participants.set(p.id, {
        id: p.id,
        name: p.name,
        role: p.role,
        authenticated: p.authenticated,
        producers: new Map(p.producers.map((pr) => [pr.id, { kind: pr.kind, source: pr.source }])),
      });
    }
    this.events.onParticipantsChanged(this.participants);

    // Set up media transports only — no capture, no producers.
    // Camera/mic are captured lazily when user explicitly enables them.
    await this.setupMedia(generation);
    if (generation !== this.generation) throw new Error('Room join cancelled');

    // Consume existing producers from participants already in the room
    if (this.media) {
      await this.consumeExistingProducers();
    }
    if (generation !== this.generation) throw new Error('Room join cancelled');
    return 'joined';
  }

  private async setupMedia(generation: number): Promise<void> {
    const media = new MediaManager(this.signaling);
    this.media = media;
    this.mediaReady = false;
    try {
      await media.setup();
    } catch (error) {
      media.close();
      if (this.media === media) { this.media = null; this.mediaReady = false; }
      if (generation === this.generation) console.warn('[room] media unavailable; chat remains connected:', error);
      return;
    }
    if (generation !== this.generation || this.media !== media) media.close();
    else this.mediaReady = true;
  }

  async leave(): Promise<void> {
    this.generation++;
    this.recoveryPromise = null;
    this.cancelJoin?.();
    this.joinPassword = undefined;
    this.hiddenParticipants.clear();
    this.videoQualities.clear();
    this.rejectSocialRequests('Room left');
    this.closeMedia();
    this.participants.clear();
    this.pausedProducers.clear();
    this.awaitingPostAdmission = false;
    this.localId = null;
    this.roomId = null;
    this.reconnectToken = null;
    this.participantName = null;
    this.connectionQuality = 'unknown';
    this.localRole = 'user';
    this._roomSettings = null;
    this.localTextMuted = false;
    this.localCamBanned = false;
    this.serverCanChat = null;
    this.serverCanBroadcast = null;
    this.recovering = false;
    this.events.onParticipantsChanged(this.participants);
    try { this.signaling.send({ type: 'leaveRoom' }); }
    catch (error) { console.warn('[room] could not signal departure:', error); }
  }

  sendChat(content: string, clientMessageId = crypto.randomUUID()): void {
    if (!this.connected) throw new Error('Reconnecting — wait before sending');
    if (!this.canChat) throw new Error('You are not allowed to chat');
    this.signaling.send({ type: 'chatMessage', content, clientMessageId });
  }

  sendPrivate(targetParticipantId: string, content: string, clientMessageId: string): void {
    if (!this.connected) throw new Error('Reconnecting — wait before sending');
    if (!this.canChat) throw new Error('You are not allowed to chat');
    this.signaling.send({ type: 'privateMessage', targetParticipantId, content, clientMessageId });
  }

  requestSocial<T = Record<string, unknown>>(action: SocialAction, data: Record<string, unknown> = {}): Promise<T> {
    if (!this.localId || !this.signaling.connected) return Promise.reject(new Error('Join a connected room first'));
    if (this.socialRequests.size >= 32) return Promise.reject(new Error('Please wait for pending actions'));
    const requestId = crypto.randomUUID();
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.socialRequests.delete(requestId);
        reject(new Error('The room did not respond; please try again'));
      }, 10_000);
      this.socialRequests.set(requestId, { action, timer, resolve: value => resolve(value as T), reject });
      try {
        this.signaling.send({ ...data, type: action, requestId });
      } catch (error) {
        clearTimeout(timer);
        this.socialRequests.delete(requestId);
        reject(error instanceof Error ? error : new Error('Unable to send room request'));
      }
    });
  }

  setCapturePreferences(preferences: CapturePreferences): void {
    this.media?.setCapturePreferences(preferences);
  }

  setRemoteMediaHidden(participantId: string, hidden: boolean): void {
    if (hidden) this.hiddenParticipants.add(participantId);
    else this.hiddenParticipants.delete(participantId);
    for (const producerId of this.participants.get(participantId)?.producers.keys() ?? []) {
      this.media?.setConsumerHiddenByProducer(producerId, hidden);
    }
  }

  setRemoteVideoQuality(participantId: string, quality: RemoteVideoQuality): void {
    this.videoQualities.set(participantId, quality);
    for (const [producerId, producer] of this.participants.get(participantId)?.producers ?? []) {
      if (producer.kind === 'video') this.media?.setConsumerQualityByProducer(producerId, quality);
    }
  }

  async toggleAudio(): Promise<boolean> {
    return await this.media?.toggleAudio() ?? false;
  }

  async toggleVideo(): Promise<boolean> {
    return await this.media?.toggleVideo() ?? false;
  }

  muteAudio(): void {
    this.media?.muteAudio();
  }

  async unmuteAudio(): Promise<void> {
    await this.media?.unmuteAudio();
  }

  get audioEnabled(): boolean {
    return this.media?.audioEnabled ?? false;
  }

  get videoEnabled(): boolean {
    return this.media?.videoEnabled ?? false;
  }

  get hasMedia(): boolean {
    return this.media !== null && this.mediaReady;
  }

  async startScreenShare(): Promise<boolean> {
    if (!this.media) return false;
    try {
      const result = await this.media.startScreenShare();
      return result !== null;
    } catch (e) {
      console.error('[room] screen share failed:', e);
      return false;
    }
  }

  stopScreenShare(): void {
    this.media?.stopScreenShare();
  }

  get isScreenSharing(): boolean {
    return this.media?.isScreenSharing ?? false;
  }

  /** Register callback for when screen share stops (browser button or explicit) */
  set onScreenShareStopped(cb: (() => void) | null) {
    if (this.media) {
      this.media.onScreenShareStopped = cb;
    }
  }

  getParticipants(): Map<string, Participant> {
    return this.participants;
  }

  getLocalStream(): MediaStream | null {
    return this.media?.getLocalStream() ?? null;
  }

  async switchCamera(deviceId: string): Promise<void> {
    await this.media?.switchCamera(deviceId);
  }

  async switchMic(deviceId: string): Promise<void> {
    await this.media?.switchMic(deviceId);
  }

  get role(): string {
    return this.localRole;
  }

  get roomSettings(): RoomSettings | null {
    return this._roomSettings;
  }

  // --- Moderation methods ---

  closeCam(targetId: string): void {
    this.signaling.send({ type: 'closeCam', targetParticipantId: targetId });
  }

  camBan(targetId: string, reason?: string): void {
    this.signaling.send({ type: 'camBan', targetParticipantId: targetId, reason });
  }

  camUnban(targetId: string): void {
    this.signaling.send({ type: 'camUnban', targetParticipantId: targetId });
  }

  textMute(targetId: string): void {
    this.signaling.send({ type: 'textMute', targetParticipantId: targetId });
  }

  textUnmute(targetId: string): void {
    this.signaling.send({ type: 'textUnmute', targetParticipantId: targetId });
  }

  kick(targetId: string, reason?: string): void {
    this.signaling.send({ type: 'kick', targetParticipantId: targetId, reason });
  }

  ban(targetId: string, reason?: string, duration?: number): void {
    this.signaling.send({ type: 'ban', targetParticipantId: targetId, reason, duration });
  }

  unban(targetUserId: string): void {
    this.signaling.send({ type: 'unban', targetUserId });
  }

  setRole(targetId: string, role: number): void {
    this.signaling.send({ type: 'setRole', targetParticipantId: targetId, role });
  }

  requestVoice(): void {
    this.signaling.send({ type: 'requestVoice' });
  }

  updateRoomSettings(settings: Partial<RoomSettings>): void {
    this.signaling.send({ type: 'updateRoomSettings', ...settings });
  }

  setTopic(topic: string): void {
    this.signaling.send({ type: 'setTopic', topic });
  }

  admitFromLobby(targetId: string): void {
    this.signaling.send({ type: 'admitFromLobby', targetParticipantId: targetId });
  }

  denyFromLobby(targetId: string): void {
    this.signaling.send({ type: 'denyFromLobby', targetParticipantId: targetId });
  }

  private attemptReconnect(): Promise<void> {
    if (this.recoveryPromise) return this.recoveryPromise;
    const task = this.reconnectSession().finally(() => {
      if (this.recoveryPromise === task) this.recoveryPromise = null;
    });
    this.recoveryPromise = task;
    return task;
  }

  private async reconnectSession(): Promise<void> {
    if (!this.localId || !this.roomId) return;
    const generation = this.generation;
    this.recovering = true;
    this.rejectSocialRequests('Connection changed; please retry');
    this.events.onRecoveryState?.('reconnecting');

    console.log('[room] attempting session reconnect...');
    try {
      const result = await this.signaling.request<
        Extract<ServerMessage, { type: 'reconnectResult' }>
      >(
        { type: 'reconnect', participantId: this.localId, roomId: this.roomId, reconnectToken: this.reconnectToken! },
        'reconnectResult',
        10000,
      );

      if (generation !== this.generation) return;

      if (result.success) {
        if (!result.reconnectToken) {
          throw new Error('Reconnect response did not rotate its credential');
        }
        this.reconnectToken = result.reconnectToken;
        this.recovering = false;
        console.log('[room] session reconnected successfully');
        try {
          await this.requestSocial('getRoomSnapshot');
          if (generation === this.generation) this.events.onRecoveryState?.('connected');
        } catch (error) {
          // A successfully resumed room remains usable even if its snapshot was
          // rate-limited. Do not destroy live transports or require another join.
          if (generation === this.generation) this.events.onRecoveryState?.('connected',
            error instanceof Error ? error.message : 'Room state could not be refreshed');
        }
        // Media transports survive independently — only signaling needed reconnection
      } else {
        console.log('[room] session expired, performing full rejoin');
        await this.fullRejoin();
      }
    } catch (e) {
      if (generation !== this.generation) return;
      console.error('[room] reconnect failed, performing full rejoin:', e);
      await this.fullRejoin();
    }
  }

  private async fullRejoin(): Promise<void> {
    if (!this.roomId || !this.participantName) return;

    const roomId = this.roomId;
    const name = this.participantName;
    const password = this.joinPassword;
    let generation = this.generation;

    // Notify UI to remove all remote tiles before rejoining
    for (const pid of this.participants.keys()) {
      this.events.onParticipantLeft(pid);
    }

    // Clean up existing media
    this.closeMedia();
    this.participants.clear();
    this.localId = null;
    this.roomId = null;

    try {
      let outcome: 'joined' | 'lobby';
      try {
        generation = this.generation + 1;
        outcome = await this.join(roomId, name, password);
      } catch (error) {
        if (generation !== this.generation) return;
        if (!(error instanceof RoomPasswordRequiredError) || !this.events.onPasswordRequired) throw error;
        const supplied = await this.events.onPasswordRequired();
        if (generation !== this.generation) return;
        if (supplied === null) throw new Error('Rejoin cancelled');
        generation = this.generation + 1;
        outcome = await this.join(roomId, name, supplied);
      }
      if (generation !== this.generation) return;
      if (outcome === 'joined') {
        this.recovering = false;
        this.events.onAdmissionComplete();
        this.events.onRecoveryState?.('connected');
      }
    } catch (e) {
      if (generation !== this.generation) return;
      console.error('[room] full rejoin failed:', e);
      this.events.onRecoveryState?.('failed', e instanceof Error ? e.message : 'Unable to rejoin');
    }
  }

  private async consumeExistingProducers(): Promise<void> {
    for (const p of this.participants.values()) {
      for (const [producerId, producer] of p.producers) {
        await this.consumeProducer(p.id, producerId, producer.kind, producer.source);
      }
    }
  }

  private async handlePostAdmission(msg: ServerMessage): Promise<void> {
    if (msg.type !== 'roomJoined' || !this.roomId) return;
    const generation = this.generation;

    this.localId = msg.participantId;
    this.reconnectToken = msg.reconnectToken;
    this.localRole = msg.yourRole ?? 'user';
    this._roomSettings = msg.roomSettings ?? null;
    this.recovering = false;

    for (const p of msg.participants) {
      this.participants.set(p.id, {
        id: p.id,
        name: p.name,
        role: p.role,
        authenticated: p.authenticated,
        producers: new Map(p.producers.map((pr) => [pr.id, { kind: pr.kind, source: pr.source }])),
      });
    }
    this.events.onParticipantsChanged(this.participants);

    await this.setupMedia(generation);
    if (generation !== this.generation) return;

    if (this.media) {
      await this.consumeExistingProducers();
    }
    if (generation !== this.generation) return;
    this.events.onAdmissionComplete();
    this.events.onRecoveryState?.('connected');
  }

  private consumeProducer(participantId: string, producerId: string, kind: 'audio' | 'video', source?: string): Promise<void> {
    const pending = this.pendingConsumes.get(producerId);
    if (pending) return pending;
    const media = this.media;
    const generation = this.generation;
    const metadata = this.participants.get(participantId)?.producers.get(producerId);
    if (!media || !this.mediaReady || !metadata) return Promise.resolve();
    const current = (): boolean => generation === this.generation && this.media === media
      && this.participants.get(participantId)?.producers.get(producerId) === metadata;
    // Signaling consumer responses have no request ID, so only one consume
    // transaction may be outstanding even when several newProducer events arrive.
    const task = this.consumeQueue.then(async () => {
      if (!current()) return;
      try {
        const track = media.getConsumerTrackByProducer?.(producerId) ?? await media.consume(producerId);
        if (!current()) { media.closeConsumerByProducer(producerId); return; }
        if (this.hiddenParticipants.has(participantId)) media.setConsumerHiddenByProducer(producerId, true);
        const quality = this.videoQualities.get(participantId);
        if (quality && kind === 'video') media.setConsumerQualityByProducer(producerId, quality);
        if (!this.pausedProducers.has(producerId)) {
          this.events.onRemoteTrack(participantId, this.participants.get(participantId)?.name ?? participantId.slice(0, 8), track, kind, source);
        }
      } catch (error) {
        if (current()) console.warn('[room] remote media unavailable:', producerId, error);
      }
    });
    this.consumeQueue = task.catch(() => {});
    this.pendingConsumes.set(producerId, task);
    void task.then(() => {
      if (this.pendingConsumes.get(producerId) === task) this.pendingConsumes.delete(producerId);
    }, () => {
      if (this.pendingConsumes.get(producerId) === task) this.pendingConsumes.delete(producerId);
    });
    return task;
  }

  private handleMessage(msg: ServerMessage): void {
    if (!this.roomId) return;
    switch (msg.type) {
      case 'roomClosed': {
        // A deleted room must never be resumed, including while waiting in its lobby.
        void this.leave();
        this.events.onRoomClosed?.(msg.reason);
        break;
      }
      case 'socialResponse': {
        const request = this.socialRequests.get(msg.requestId);
        if (!request || request.action !== msg.action) break;
        this.socialRequests.delete(msg.requestId);
        clearTimeout(request.timer);
        try {
          if (msg.action === 'getRoomSnapshot') this.applySnapshot(msg.data as unknown as RoomSnapshot);
          this.events.onSocialEvent?.(msg);
          request.resolve(msg.data);
        } catch (error) {
          request.reject(error instanceof Error ? error : new Error('Room response could not be applied'));
        }
        break;
      }
      case 'socialError': {
        const request = msg.requestId ? this.socialRequests.get(msg.requestId) : undefined;
        if (request && msg.requestId) {
          this.socialRequests.delete(msg.requestId);
          clearTimeout(request.timer);
          request.reject(new Error(msg.message));
        }
        this.events.onSocialEvent?.(msg);
        break;
      }
      case 'messageAck':
      case 'privateMessageReceived': {
        this.events.onSocialEvent?.(msg);
        break;
      }
      case 'nicknameChanged': {
        if (msg.participantId === this.localId) this.participantName = msg.nickname;
        const participant = this.participants.get(msg.participantId);
        if (participant) participant.name = msg.nickname;
        this.events.onParticipantsChanged(this.participants);
        this.events.onSocialEvent?.(msg);
        break;
      }
      case 'participantJoined': {
        this.participants.set(msg.participantId, {
          id: msg.participantId,
          name: msg.participantName,
          role: msg.role,
          authenticated: msg.authenticated,
          producers: new Map(),
        });
        this.events.onParticipantJoined(msg.participantId, msg.participantName);
        this.events.onParticipantsChanged(this.participants);
        break;
      }
      case 'participantLeft': {
        this.hiddenParticipants.delete(msg.participantId);
        this.videoQualities.delete(msg.participantId);
        // Clean up paused state for this participant's producers
        const leaving = this.participants.get(msg.participantId);
        if (leaving) {
          for (const producerId of leaving.producers.keys()) {
            this.pausedProducers.delete(producerId);
            this.media?.closeConsumerByProducer(producerId);
          }
        }
        this.participants.delete(msg.participantId);
        this.events.onParticipantLeft(msg.participantId);
        this.events.onParticipantsChanged(this.participants);
        break;
      }
      case 'newProducer': {
        const participant = this.participants.get(msg.participantId);
        if (!participant) break;
        if (!participant.producers.has(msg.producerId)) {
          participant.producers.set(msg.producerId, { kind: msg.kind, source: msg.source });
        }
        this.events.onParticipantsChanged(this.participants);
        // Auto-consume the new producer
        void this.consumeProducer(msg.participantId, msg.producerId, msg.kind, msg.source);
        break;
      }
      case 'producerClosed': {
        this.pausedProducers.delete(msg.producerId);
        if (this.media?.closeLocalProducer(msg.producerId)) {
          this.events.onLocalMediaChanged();
        }
        // Find which participant owned this producer and close the consumer
        for (const [pid, p] of this.participants) {
          if (p.producers.has(msg.producerId)) {
            const meta = p.producers.get(msg.producerId)!;
            p.producers.delete(msg.producerId);
            this.media?.closeConsumerByProducer(msg.producerId);
            this.events.onRemoteTrackRemoved(pid, msg.producerId, meta.kind, meta.source);
            break;
          }
        }
        this.events.onParticipantsChanged(this.participants);
        break;
      }
      case 'iceRestarted': {
        this.media?.handleIceRestarted(msg.transportId, msg.iceParameters);
        break;
      }
      case 'connectionStats': {
        const bitrate = msg.availableBitrate ?? 0;
        if (bitrate > 500_000) {
          this.connectionQuality = 'good';
        } else if (bitrate > 200_000) {
          this.connectionQuality = 'fair';
        } else {
          this.connectionQuality = 'poor';
        }
        this.events.onConnectionQuality(this.connectionQuality);
        break;
      }
      case 'consumerLayersChanged': {
        // Informational — could show layer info in UI
        console.log(`[room] consumer ${msg.consumerId} layers: spatial=${msg.spatialLayer}, temporal=${msg.temporalLayer}`);
        break;
      }
      case 'chatReceived': {
        this.events.onChatMessage(msg.participantId, msg.participantName, msg.content);
        this.events.onSocialEvent?.(msg);
        break;
      }
      case 'producerPaused': {
        this.pausedProducers.add(msg.producerId);
        for (const [pid, p] of this.participants) {
          if (p.producers.has(msg.producerId)) {
            const meta = p.producers.get(msg.producerId)!;
            console.log(`[room] producer ${msg.producerId} paused → hiding ${meta.kind} tile`);
            this.events.onRemoteTrackRemoved(pid, msg.producerId, meta.kind, meta.source);
            break;
          }
        }
        break;
      }
      case 'producerResumed': {
        this.pausedProducers.delete(msg.producerId);
        for (const [pid, p] of this.participants) {
          if (p.producers.has(msg.producerId)) {
            const meta = p.producers.get(msg.producerId)!;
            console.log(`[room] producer ${msg.producerId} resumed → showing ${meta.kind} tile`);
            const track = this.media?.getConsumerTrackByProducer(msg.producerId);
            if (track) {
              this.events.onRemoteTrack(pid, p.name, track, meta.kind, meta.source);
            }
            break;
          }
        }
        break;
      }
      case 'activeSpeaker': {
        this.events.onActiveSpeaker(msg.participantId);
        break;
      }
      case 'audioLevels': {
        this.events.onAudioLevels(msg.levels);
        break;
      }
      // Moderation broadcasts
      case 'forceClosedProducer': {
        // Handle same as producerClosed — remove from participants and clean up
        this.pausedProducers.delete(msg.producerId);
        if (this.media?.closeLocalProducer(msg.producerId)) {
          this.events.onLocalMediaChanged();
        }
        for (const [pid, p] of this.participants) {
          if (p.producers.has(msg.producerId)) {
            const meta = p.producers.get(msg.producerId)!;
            p.producers.delete(msg.producerId);
            this.media?.closeConsumerByProducer(msg.producerId);
            this.events.onRemoteTrackRemoved(pid, msg.producerId, meta.kind, meta.source);
            break;
          }
        }
        this.events.onParticipantsChanged(this.participants);
        break;
      }
      case 'camBanned': {
        if (msg.participantId === this.localId) this.localCamBanned = true;
        this.events.onModeration('camBanned', msg.participantId);
        break;
      }
      case 'camUnbanned': {
        if (msg.participantId === this.localId) this.localCamBanned = false;
        this.events.onModeration('camUnbanned', msg.participantId);
        break;
      }
      case 'textMuted': {
        if (msg.participantId === this.localId) { this.localTextMuted = true; this.serverCanChat = null; }
        this.events.onModeration('textMuted', msg.participantId);
        break;
      }
      case 'textUnmuted': {
        if (msg.participantId === this.localId) { this.localTextMuted = false; this.serverCanChat = null; }
        this.events.onModeration('textUnmuted', msg.participantId);
        break;
      }
      case 'participantKicked': {
        this.events.onModeration('kicked', msg.participantId, msg.reason);
        // If it's us, leave the room
        if (msg.participantId === this.localId) {
          void this.leave();
        }
        break;
      }
      case 'participantBanned': {
        this.events.onModeration('banned', msg.participantId, msg.reason);
        if (msg.participantId === this.localId) {
          void this.leave();
        }
        break;
      }
      case 'roleChanged': {
        if (msg.participantId === this.localId) {
          this.localRole = msg.newRole;
          this.serverCanChat = null;
          this.serverCanBroadcast = null;
        }
        const roleTarget = this.participants.get(msg.participantId);
        if (roleTarget) {
          roleTarget.role = msg.newRole;
        }
        this.events.onRoleChanged(msg.participantId, msg.newRole);
        break;
      }
      case 'voiceRequested': {
        this.events.onVoiceRequested(msg.participantId, msg.displayName);
        break;
      }
      // Room state
      case 'roomSettingsChanged': {
        this._roomSettings = msg.settings;
        this.serverCanChat = null;
        this.serverCanBroadcast = null;
        this.events.onRoomSettingsChanged(msg.settings);
        break;
      }
      case 'topicChanged': {
        if (this._roomSettings) {
          this._roomSettings.topic = msg.topic;
        }
        this.events.onTopicChanged(msg.topic, msg.changedBy);
        break;
      }
      // Lobby
      case 'lobbyWaiting': {
        this.events.onLobbyWaiting(msg.roomName, msg.topic, msg.participantCount);
        break;
      }
      case 'lobbyJoin': {
        this.events.onLobbyJoin(msg.participantId, msg.displayName);
        break;
      }
      case 'lobbyAdmitted': {
        this.awaitingPostAdmission = true;
        this.events.onLobbyAdmitted();
        break;
      }
      case 'lobbyDenied': {
        this.events.onLobbyDenied(msg.reason);
        break;
      }
      case 'roomJoined': {
        if (!this.awaitingPostAdmission) break;
        this.awaitingPostAdmission = false;
        void this.handlePostAdmission(msg);
        break;
      }
      default:
        break;
    }
  }

  private applySnapshot(snapshot: RoomSnapshot): void {
    if (!snapshot || !Array.isArray(snapshot.participants) || typeof snapshot.yourRole !== 'string'
      || snapshot.participants.some(p => !p || typeof p.id !== 'string' || typeof p.name !== 'string'
        || !Array.isArray(p.producers) || p.producers.some(producer => !producer
          || typeof producer.id !== 'string' || !['audio', 'video'].includes(producer.kind)))) {
      throw new Error('The room snapshot was incomplete');
    }
    const oldPaused = this.pausedProducers;
    this.pausedProducers = new Set(snapshot.pausedProducerIds ?? oldPaused);
    const incoming = new Map(snapshot.participants.filter(p => p.id !== this.localId).map(p => [p.id, p]));
    for (const [id, previous] of this.participants) {
      const current = incoming.get(id);
      for (const [producerId, metadata] of previous.producers) {
        if (!current?.producers.some(producer => producer.id === producerId)) {
          this.media?.closeConsumerByProducer(producerId);
          this.pausedProducers.delete(producerId);
          this.events.onRemoteTrackRemoved(id, producerId, metadata.kind, metadata.source);
        }
      }
      if (!current) {
        this.participants.delete(id);
        this.hiddenParticipants.delete(id);
        this.videoQualities.delete(id);
        this.events.onParticipantLeft(id);
      }
    }
    for (const info of incoming.values()) {
      const previous = this.participants.get(info.id);
      this.participants.set(info.id, { id: info.id, name: info.name, role: info.role, authenticated: info.authenticated,
        producers: new Map(info.producers.map(producer => {
          const old = previous?.producers.get(producer.id);
          return [producer.id, old?.kind === producer.kind && old.source === producer.source
            ? old : { kind: producer.kind, source: producer.source }];
        })) });
    }
    const oldRole = this.localRole;
    const oldTextMuted = this.localTextMuted;
    const oldCamBanned = this.localCamBanned;
    this.localRole = snapshot.yourRole;
    this._roomSettings = snapshot.roomSettings ?? null;
    if (typeof snapshot.nickname === 'string') this.participantName = snapshot.nickname;
    this.localTextMuted = snapshot.textMuted ?? this.localTextMuted;
    this.localCamBanned = snapshot.camBanned ?? this.localCamBanned;
    this.serverCanChat = snapshot.canChat ?? null;
    this.serverCanBroadcast = snapshot.canBroadcast ?? null;
    if (snapshot.localProducerIds && this.media?.reconcileLocalProducers(snapshot.localProducerIds)) {
      this.events.onLocalMediaChanged();
    }
    this.events.onParticipantsChanged(this.participants);
    if (oldRole !== this.localRole && this.localId) this.events.onRoleChanged(this.localId, this.localRole);
    if (oldTextMuted !== this.localTextMuted && this.localId) this.events.onModeration(this.localTextMuted ? 'textMuted' : 'textUnmuted', this.localId);
    if (oldCamBanned !== this.localCamBanned && this.localId) this.events.onModeration(this.localCamBanned ? 'camBanned' : 'camUnbanned', this.localId);
    if (this._roomSettings) this.events.onRoomSettingsChanged(this._roomSettings);
    for (const participant of this.participants.values()) {
      for (const [producerId, metadata] of participant.producers) {
        if (this.pausedProducers.has(producerId)) {
          if (!oldPaused.has(producerId)) this.events.onRemoteTrackRemoved(participant.id, producerId, metadata.kind, metadata.source);
        } else if (oldPaused.has(producerId)) {
          const track = this.media?.getConsumerTrackByProducer(producerId);
          if (track) this.events.onRemoteTrack(participant.id, participant.name, track, metadata.kind, metadata.source);
        }
        // Reuse existing consumers, and retry previously missing ones. Metadata
        // identity and membership generation prevent late work restoring a tile.
        if (!this.media?.getConsumerTrackByProducer(producerId)) {
          void this.consumeProducer(participant.id, producerId, metadata.kind, metadata.source);
        }
      }
    }
  }
}
