import type { ParticipantInfo, ServerMessage } from './protocol';
import { SignalingClient } from './signaling';
import { MediaManager } from './media';

export interface Participant {
  id: string;
  name: string;
  producers: Map<string, { kind: 'audio' | 'video'; source?: string }>;
}

export type ConnectionQuality = 'good' | 'fair' | 'poor' | 'unknown';

export type RoomEventHandler = {
  onParticipantsChanged: (participants: Map<string, Participant>) => void;
  onLocalStream: (stream: MediaStream) => void;
  onRemoteTrack: (participantId: string, participantName: string, track: MediaStreamTrack, kind: 'audio' | 'video', source?: string) => void;
  onRemoteTrackRemoved: (participantId: string, producerId: string, kind: 'audio' | 'video', source?: string) => void;
  onParticipantLeft: (participantId: string) => void;
  onParticipantJoined: (participantId: string, participantName: string) => void;
  onChatMessage: (participantId: string, participantName: string, content: string) => void;
  onConnectionQuality: (quality: ConnectionQuality) => void;
  onActiveSpeaker: (participantId: string) => void;
  onAudioLevels: (levels: { participantId: string; volume: number }[]) => void;
};

export class RoomClient {
  private signaling: SignalingClient;
  private media: MediaManager | null = null;
  private participants = new Map<string, Participant>();
  private localId: string | null = null;
  private roomId: string | null = null;
  private participantName: string | null = null;
  private events: RoomEventHandler;
  private reconnectToken: string | null = null;
  private connectionQuality: ConnectionQuality = 'unknown';
  // Producers known to be paused — prevents showing black tiles from race condition
  // where ProducerPaused arrives before consumeProducer finishes
  private pausedProducers = new Set<string>();

  constructor(signaling: SignalingClient, events: RoomEventHandler) {
    this.signaling = signaling;
    this.events = events;
    this.signaling.setOnMessage((msg) => this.handleMessage(msg));
    this.signaling.setOnReconnected(() => this.attemptReconnect());
  }

  get localParticipantId(): string | null {
    return this.localId;
  }

  get currentRoomId(): string | null {
    return this.roomId;
  }

  async join(roomId: string, participantName: string): Promise<void> {
    this.roomId = roomId;
    this.participantName = participantName;

    // Join room
    const response = await this.signaling.request<
      Extract<ServerMessage, { type: 'roomJoined' }>
    >({ type: 'joinRoom', roomId, participantName }, 'roomJoined');

    this.localId = response.participantId;
    this.reconnectToken = response.reconnectToken;

    // Store existing participants
    for (const p of response.participants) {
      this.participants.set(p.id, {
        id: p.id,
        name: p.name,
        producers: new Map(p.producers.map((pr) => [pr.id, { kind: pr.kind, source: pr.source }])),
      });
    }
    this.events.onParticipantsChanged(this.participants);

    // Set up media transports only — no capture, no producers.
    // Camera/mic are captured lazily when user explicitly enables them.
    this.media = new MediaManager(this.signaling);
    try {
      await this.media.setup();
    } catch (e) {
      // Media setup failed (e.g. not HTTPS)
      // Stay in room for chat — media is optional
      console.warn('[room] media setup failed, continuing without camera/mic:', e);
      this.media.close();
      this.media = null;
    }

    // Consume existing producers from participants already in the room
    if (this.media) {
      await this.consumeExistingProducers(response.participants);
    }
  }

  async leave(): Promise<void> {
    this.media?.stopScreenShare();
    this.media?.close();
    this.media = null;
    this.signaling.send({ type: 'leaveRoom' });
    this.participants.clear();
    this.pausedProducers.clear();
    this.localId = null;
    this.roomId = null;
    this.reconnectToken = null;
    this.participantName = null;
    this.connectionQuality = 'unknown';
    this.events.onParticipantsChanged(this.participants);
  }

  sendChat(content: string): void {
    this.signaling.send({ type: 'chatMessage', content });
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
    return this.media !== null;
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

  private async attemptReconnect(): Promise<void> {
    if (!this.localId || !this.roomId) return;

    console.log('[room] attempting session reconnect...');
    try {
      const result = await this.signaling.request<
        Extract<ServerMessage, { type: 'reconnectResult' }>
      >(
        { type: 'reconnect', participantId: this.localId, roomId: this.roomId, reconnectToken: this.reconnectToken! },
        'reconnectResult',
        10000,
      );

      if (result.success) {
        console.log('[room] session reconnected successfully');
        // Media transports survive independently — only signaling needed reconnection
      } else {
        console.log('[room] session expired, performing full rejoin');
        await this.fullRejoin();
      }
    } catch (e) {
      console.error('[room] reconnect failed, performing full rejoin:', e);
      await this.fullRejoin();
    }
  }

  private async fullRejoin(): Promise<void> {
    if (!this.roomId || !this.participantName) return;

    const roomId = this.roomId;
    const name = this.participantName;

    // Notify UI to remove all remote tiles before rejoining
    for (const pid of this.participants.keys()) {
      this.events.onParticipantLeft(pid);
    }

    // Clean up existing media
    this.media?.close();
    this.media = null;
    this.participants.clear();
    this.localId = null;
    this.roomId = null;

    try {
      await this.join(roomId, name);
    } catch (e) {
      console.error('[room] full rejoin failed:', e);
    }
  }

  private async consumeExistingProducers(participants: ParticipantInfo[]): Promise<void> {
    for (const p of participants) {
      for (const producer of p.producers) {
        await this.consumeProducer(p.id, producer.id, producer.kind, producer.source);
      }
    }
  }

  private async consumeProducer(participantId: string, producerId: string, kind: 'audio' | 'video', source?: string): Promise<void> {
    if (!this.media) return;
    const participant = this.participants.get(participantId);
    const name = participant?.name ?? participantId.slice(0, 8);
    try {
      const track = await this.media.consume(producerId);
      // Skip rendering if producer is paused (user has mic/cam off).
      // ProducerPaused may arrive before consume finishes — the set handles this race.
      if (!this.pausedProducers.has(producerId)) {
        this.events.onRemoteTrack(participantId, name, track, kind, source);
      }
    } catch (e) {
      console.error(`Failed to consume producer ${producerId}:`, e);
    }
  }

  private handleMessage(msg: ServerMessage): void {
    switch (msg.type) {
      case 'participantJoined': {
        this.participants.set(msg.participantId, {
          id: msg.participantId,
          name: msg.participantName,
          producers: new Map(),
        });
        this.events.onParticipantJoined(msg.participantId, msg.participantName);
        this.events.onParticipantsChanged(this.participants);
        break;
      }
      case 'participantLeft': {
        // Clean up paused state for this participant's producers
        const leaving = this.participants.get(msg.participantId);
        if (leaving) {
          for (const producerId of leaving.producers.keys()) {
            this.pausedProducers.delete(producerId);
          }
        }
        this.participants.delete(msg.participantId);
        this.events.onParticipantLeft(msg.participantId);
        this.events.onParticipantsChanged(this.participants);
        break;
      }
      case 'newProducer': {
        const participant = this.participants.get(msg.participantId);
        if (participant) {
          participant.producers.set(msg.producerId, { kind: msg.kind, source: msg.source });
        }
        this.events.onParticipantsChanged(this.participants);
        // Auto-consume the new producer
        void this.consumeProducer(msg.participantId, msg.producerId, msg.kind, msg.source);
        break;
      }
      case 'producerClosed': {
        this.pausedProducers.delete(msg.producerId);
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
      default:
        break;
    }
  }
}
