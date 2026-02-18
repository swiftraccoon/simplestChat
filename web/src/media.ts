import * as mediasoupClient from 'mediasoup-client';
import type { ServerMessage } from './protocol';
import type { SignalingClient } from './signaling';

export class MediaManager {
  private signaling: SignalingClient;
  private device: mediasoupClient.Device | null = null;
  private sendTransport: mediasoupClient.types.Transport | null = null;
  private recvTransport: mediasoupClient.types.Transport | null = null;
  private audioProducer: mediasoupClient.types.Producer | null = null;
  private videoProducer: mediasoupClient.types.Producer | null = null;
  private consumers = new Map<string, mediasoupClient.types.Consumer>();
  // Map producerId → consumerId for cleanup when producer closes
  private producerToConsumer = new Map<string, string>();
  private localStream: MediaStream | null = null;
  // ICE restart timers per transport
  private iceRestartTimers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(signaling: SignalingClient) {
    this.signaling = signaling;
  }

  get audioEnabled(): boolean {
    return this.audioProducer?.paused === false;
  }

  get videoEnabled(): boolean {
    return this.videoProducer?.paused === false;
  }

  /** Load device, create transports */
  async setup(): Promise<void> {
    // 1. Get router RTP capabilities
    const capsResponse = await this.signaling.request<
      Extract<ServerMessage, { type: 'routerRtpCapabilities' }>
    >({ type: 'getRouterRtpCapabilities' }, 'routerRtpCapabilities');

    // 2. Load device
    this.device = new mediasoupClient.Device();
    await this.device.load({ routerRtpCapabilities: capsResponse.rtpCapabilities });
    console.log('[media] device loaded');

    // 3. Create send transport
    const sendResponse = await this.signaling.request<
      Extract<ServerMessage, { type: 'transportCreated' }>
    >({ type: 'createSendTransport' }, 'transportCreated');

    this.sendTransport = this.device.createSendTransport({
      id: sendResponse.transportId,
      iceParameters: sendResponse.iceParameters,
      iceCandidates: sendResponse.iceCandidates,
      dtlsParameters: sendResponse.dtlsParameters,
      iceServers: sendResponse.iceServers,
    });

    this.sendTransport.on('connect', ({ dtlsParameters }, callback, errback) => {
      this.signaling
        .request(
          { type: 'connectTransport', transportId: this.sendTransport!.id, dtlsParameters },
          'transportConnected',
        )
        .then(() => callback())
        .catch(errback);
    });

    this.sendTransport.on('produce', async ({ kind, rtpParameters }, callback, errback) => {
      try {
        const resp = await this.signaling.request<
          Extract<ServerMessage, { type: 'producerCreated' }>
        >(
          { type: 'produce', transportId: this.sendTransport!.id, kind, rtpParameters },
          'producerCreated',
        );
        callback({ id: resp.producerId });
      } catch (e) {
        errback(e as Error);
      }
    });

    this.setupIceRecovery(this.sendTransport);

    console.log('[media] send transport created:', this.sendTransport.id);

    // 4. Create recv transport
    const recvResponse = await this.signaling.request<
      Extract<ServerMessage, { type: 'transportCreated' }>
    >({ type: 'createRecvTransport' }, 'transportCreated');

    this.recvTransport = this.device.createRecvTransport({
      id: recvResponse.transportId,
      iceParameters: recvResponse.iceParameters,
      iceCandidates: recvResponse.iceCandidates,
      dtlsParameters: recvResponse.dtlsParameters,
      iceServers: recvResponse.iceServers,
    });

    this.recvTransport.on('connect', ({ dtlsParameters }, callback, errback) => {
      this.signaling
        .request(
          { type: 'connectTransport', transportId: this.recvTransport!.id, dtlsParameters },
          'transportConnected',
        )
        .then(() => callback())
        .catch(errback);
    });

    this.setupIceRecovery(this.recvTransport);

    console.log('[media] recv transport created:', this.recvTransport.id);
  }

  /** Monitor transport connection state and request ICE restart on failure */
  private setupIceRecovery(transport: mediasoupClient.types.Transport): void {
    transport.on('connectionstatechange', (state: string) => {
      console.log(`[media] transport ${transport.id} connection state: ${state}`);

      if (state === 'disconnected') {
        // Wait 3 seconds before requesting ICE restart
        if (!this.iceRestartTimers.has(transport.id)) {
          const timer = setTimeout(() => {
            this.iceRestartTimers.delete(transport.id);
            console.log(`[media] requesting ICE restart for transport ${transport.id}`);
            this.signaling.send({ type: 'restartIce', transportId: transport.id });
          }, 3000);
          this.iceRestartTimers.set(transport.id, timer);
        }
      } else if (state === 'failed') {
        // Clear any pending timer and restart immediately
        const timer = this.iceRestartTimers.get(transport.id);
        if (timer) {
          clearTimeout(timer);
          this.iceRestartTimers.delete(transport.id);
        }
        console.log(`[media] requesting immediate ICE restart for transport ${transport.id}`);
        this.signaling.send({ type: 'restartIce', transportId: transport.id });
      } else if (state === 'connected') {
        // Clear pending restart timer if connection recovered on its own
        const timer = this.iceRestartTimers.get(transport.id);
        if (timer) {
          clearTimeout(timer);
          this.iceRestartTimers.delete(transport.id);
        }
      }
    });
  }

  /** Handle ICE restarted response from server */
  handleIceRestarted(transportId: string, iceParameters: unknown): void {
    const transport = this.sendTransport?.id === transportId
      ? this.sendTransport
      : this.recvTransport?.id === transportId
        ? this.recvTransport
        : null;

    if (transport) {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      transport.restartIce({ iceParameters: iceParameters as any });
      console.log(`[media] ICE restarted for transport ${transportId}`);
    }
  }

  /** Capture camera + mic, produce both tracks */
  async startCapture(): Promise<MediaStream> {
    this.localStream = await navigator.mediaDevices.getUserMedia({
      audio: true,
      video: { width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } },
    });

    if (!this.sendTransport) throw new Error('Send transport not ready');

    // Produce audio
    const audioTrack = this.localStream.getAudioTracks()[0];
    if (audioTrack) {
      this.audioProducer = await this.sendTransport.produce({ track: audioTrack });
      console.log('[media] audio producer created:', this.audioProducer.id);
    }

    // Produce video with simulcast (3 quality layers)
    const videoTrack = this.localStream.getVideoTracks()[0];
    if (videoTrack) {
      this.videoProducer = await this.sendTransport.produce({
        track: videoTrack,
        encodings: [
          { rid: 'r0', maxBitrate: 100_000, scaleResolutionDownBy: 4 },
          { rid: 'r1', maxBitrate: 300_000, scaleResolutionDownBy: 2 },
          { rid: 'r2', maxBitrate: 900_000 },
        ],
        codecOptions: { videoGoogleStartBitrate: 1000 },
      });
      console.log('[media] video producer created (simulcast):', this.videoProducer.id);
    }

    return this.localStream;
  }

  /** Consume a remote producer, returns the track */
  async consume(producerId: string): Promise<MediaStreamTrack> {
    if (!this.device || !this.recvTransport) {
      throw new Error('Device/transport not ready');
    }

    const response = await this.signaling.request<
      Extract<ServerMessage, { type: 'consumerCreated' }>
    >(
      { type: 'consume', producerId, rtpCapabilities: this.device.rtpCapabilities },
      'consumerCreated',
    );

    const consumer = await this.recvTransport.consume({
      id: response.consumerId,
      producerId: response.producerId,
      kind: response.kind,
      rtpParameters: response.rtpParameters,
    });

    this.consumers.set(response.consumerId, consumer);
    this.producerToConsumer.set(producerId, response.consumerId);

    // Resume the consumer on the server (created paused)
    await this.signaling.request(
      { type: 'resumeConsumer', consumerId: response.consumerId },
      'consumerResumed',
    );

    console.log(`[media] consuming ${response.kind} from producer ${producerId}`);
    return consumer.track;
  }

  /** Set preferred simulcast layers for a consumer */
  setPreferredLayers(consumerId: string, spatialLayer: number, temporalLayer?: number): void {
    this.signaling.send({
      type: 'setConsumerPreferredLayers',
      consumerId,
      spatialLayer,
      ...(temporalLayer !== undefined && { temporalLayer }),
    });
  }

  /** Close and remove the consumer for a given producer */
  closeConsumerByProducer(producerId: string): void {
    const consumerId = this.producerToConsumer.get(producerId);
    if (!consumerId) return;
    const consumer = this.consumers.get(consumerId);
    if (consumer) {
      consumer.close();
      this.consumers.delete(consumerId);
    }
    this.producerToConsumer.delete(producerId);
  }

  getLocalStream(): MediaStream | null {
    return this.localStream;
  }

  /** Toggle local audio (pause/resume producer) */
  toggleAudio(): boolean {
    if (!this.audioProducer) return false;
    if (this.audioProducer.paused) {
      this.audioProducer.resume();
      this.signaling.send({ type: 'resumeProducer', producerId: this.audioProducer.id });
    } else {
      this.audioProducer.pause();
      this.signaling.send({ type: 'pauseProducer', producerId: this.audioProducer.id });
    }
    return !this.audioProducer.paused;
  }

  /** Explicitly mute audio (idempotent) */
  muteAudio(): void {
    if (this.audioProducer && !this.audioProducer.paused) {
      this.audioProducer.pause();
      this.signaling.send({ type: 'pauseProducer', producerId: this.audioProducer.id });
    }
  }

  /** Explicitly unmute audio (idempotent) */
  unmuteAudio(): void {
    if (this.audioProducer && this.audioProducer.paused) {
      this.audioProducer.resume();
      this.signaling.send({ type: 'resumeProducer', producerId: this.audioProducer.id });
    }
  }

  /** Toggle local video (pause/resume producer) */
  toggleVideo(): boolean {
    if (!this.videoProducer) return false;
    if (this.videoProducer.paused) {
      this.videoProducer.resume();
      this.signaling.send({ type: 'resumeProducer', producerId: this.videoProducer.id });
    } else {
      this.videoProducer.pause();
      this.signaling.send({ type: 'pauseProducer', producerId: this.videoProducer.id });
    }
    return !this.videoProducer.paused;
  }

  /** Switch to a different camera device */
  async switchCamera(deviceId: string): Promise<void> {
    if (!this.videoProducer) return;

    const newStream = await navigator.mediaDevices.getUserMedia({
      video: { deviceId: { exact: deviceId }, width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } },
    });
    const newTrack = newStream.getVideoTracks()[0];
    if (!newTrack) return;

    // Replace track on producer
    await this.videoProducer.replaceTrack({ track: newTrack });

    // Stop old track and update local stream
    const oldTrack = this.localStream?.getVideoTracks()[0];
    if (oldTrack) {
      oldTrack.stop();
      this.localStream?.removeTrack(oldTrack);
    }
    this.localStream?.addTrack(newTrack);
    console.log('[media] camera switched to:', deviceId);
  }

  /** Switch to a different microphone device */
  async switchMic(deviceId: string): Promise<void> {
    if (!this.audioProducer) return;

    const newStream = await navigator.mediaDevices.getUserMedia({
      audio: { deviceId: { exact: deviceId } },
    });
    const newTrack = newStream.getAudioTracks()[0];
    if (!newTrack) return;

    // Replace track on producer
    await this.audioProducer.replaceTrack({ track: newTrack });

    // Stop old track and update local stream
    const oldTrack = this.localStream?.getAudioTracks()[0];
    if (oldTrack) {
      oldTrack.stop();
      this.localStream?.removeTrack(oldTrack);
    }
    this.localStream?.addTrack(newTrack);
    console.log('[media] microphone switched to:', deviceId);
  }

  close(): void {
    // Clear ICE restart timers
    for (const timer of this.iceRestartTimers.values()) {
      clearTimeout(timer);
    }
    this.iceRestartTimers.clear();

    for (const consumer of this.consumers.values()) {
      consumer.close();
    }
    this.consumers.clear();
    this.producerToConsumer.clear();

    this.audioProducer?.close();
    this.videoProducer?.close();
    this.audioProducer = null;
    this.videoProducer = null;

    this.sendTransport?.close();
    this.recvTransport?.close();
    this.sendTransport = null;
    this.recvTransport = null;
    this.device = null;

    // Stop local tracks
    if (this.localStream) {
      for (const track of this.localStream.getTracks()) {
        track.stop();
      }
      this.localStream = null;
    }
  }
}
