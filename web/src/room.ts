import type { ParticipantInfo, ServerMessage } from './protocol';
import { SignalingClient } from './signaling';
import { MediaManager } from './media';

export interface Participant {
  id: string;
  name: string;
  producers: Map<string, 'audio' | 'video'>;
}

export type ConnectionQuality = 'good' | 'fair' | 'poor' | 'unknown';

export type RoomEventHandler = {
  onParticipantsChanged: (participants: Map<string, Participant>) => void;
  onLocalStream: (stream: MediaStream) => void;
  onRemoteTrack: (participantId: string, participantName: string, track: MediaStreamTrack, kind: 'audio' | 'video') => void;
  onRemoteTrackRemoved: (participantId: string, producerId: string, kind: 'audio' | 'video') => void;
  onParticipantLeft: (participantId: string) => void;
  onParticipantJoined: (participantId: string, participantName: string) => void;
  onChatMessage: (participantId: string, participantName: string, content: string) => void;
  onConnectionQuality: (quality: ConnectionQuality) => void;
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
        producers: new Map(p.producers.map((pr) => [pr.id, pr.kind])),
      });
    }
    this.events.onParticipantsChanged(this.participants);

    // Set up media (device, transports)
    this.media = new MediaManager(this.signaling);
    try {
      await this.media.setup();
      const localStream = await this.media.startCapture();
      // Privacy: join muted with cam off — user must explicitly enable
      this.media.muteAudio();
      this.media.pauseVideo();
      this.events.onLocalStream(localStream);
    } catch (e) {
      // Media setup failed (e.g. camera denied, not HTTPS)
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
    this.media?.close();
    this.media = null;
    this.signaling.send({ type: 'leaveRoom' });
    this.participants.clear();
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

  toggleAudio(): boolean {
    return this.media?.toggleAudio() ?? false;
  }

  toggleVideo(): boolean {
    return this.media?.toggleVideo() ?? false;
  }

  muteAudio(): void {
    this.media?.muteAudio();
  }

  unmuteAudio(): void {
    this.media?.unmuteAudio();
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
        await this.consumeProducer(p.id, producer.id, producer.kind);
      }
    }
  }

  private async consumeProducer(participantId: string, producerId: string, kind: 'audio' | 'video'): Promise<void> {
    if (!this.media) return;
    const participant = this.participants.get(participantId);
    const name = participant?.name ?? participantId.slice(0, 8);
    try {
      const track = await this.media.consume(producerId);
      this.events.onRemoteTrack(participantId, name, track, kind);
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
        this.participants.delete(msg.participantId);
        this.events.onParticipantLeft(msg.participantId);
        this.events.onParticipantsChanged(this.participants);
        break;
      }
      case 'newProducer': {
        const participant = this.participants.get(msg.participantId);
        if (participant) {
          participant.producers.set(msg.producerId, msg.kind);
        }
        this.events.onParticipantsChanged(this.participants);
        // Auto-consume the new producer
        void this.consumeProducer(msg.participantId, msg.producerId, msg.kind);
        break;
      }
      case 'producerClosed': {
        // Find which participant owned this producer and close the consumer
        for (const [pid, p] of this.participants) {
          if (p.producers.has(msg.producerId)) {
            const kind = p.producers.get(msg.producerId)!;
            p.producers.delete(msg.producerId);
            this.media?.closeConsumerByProducer(msg.producerId);
            this.events.onRemoteTrackRemoved(pid, msg.producerId, kind);
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
        for (const p of this.participants.values()) {
          if (p.producers.has(msg.producerId)) {
            console.log(`[room] producer ${msg.producerId} paused (remote mute)`);
            break;
          }
        }
        break;
      }
      case 'producerResumed': {
        for (const p of this.participants.values()) {
          if (p.producers.has(msg.producerId)) {
            console.log(`[room] producer ${msg.producerId} resumed (remote unmute)`);
            break;
          }
        }
        break;
      }
      default:
        break;
    }
  }
}
