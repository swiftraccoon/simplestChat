import * as mediasoupClient from 'mediasoup-client';
import type { ServerMessage } from './protocol';
import type { SignalingClient } from './signaling';

export interface CapturePreferences {
  cameraDeviceId: string;
  microphoneDeviceId: string;
  resolution: '360p' | '720p' | '1080p';
  frameRate: 15 | 30 | 60;
  echoCancellation: boolean;
  autoGainControl: boolean;
  noiseSuppression: boolean;
}

export type RemoteVideoQuality = 'auto' | 'low' | 'medium' | 'high';

export const DEFAULT_CAPTURE_PREFERENCES: CapturePreferences = {
  cameraDeviceId: '', microphoneDeviceId: '', resolution: '720p', frameRate: 30,
  echoCancellation: true, autoGainControl: true, noiseSuppression: true,
};

const CAPTURE_STORAGE_KEY = 'simplestchat.capturePreferences';

export function normalizeCapturePreferences(value: unknown): CapturePreferences {
  const preferences = value && typeof value === 'object' ? value as Record<string, unknown> : {};
  return {
    cameraDeviceId: typeof preferences.cameraDeviceId === 'string' ? preferences.cameraDeviceId : '',
    microphoneDeviceId: typeof preferences.microphoneDeviceId === 'string' ? preferences.microphoneDeviceId : '',
    resolution: preferences.resolution === '360p' || preferences.resolution === '1080p' ? preferences.resolution : '720p',
    frameRate: preferences.frameRate === 15 || preferences.frameRate === 60 ? preferences.frameRate : 30,
    echoCancellation: preferences.echoCancellation !== false,
    autoGainControl: preferences.autoGainControl !== false,
    noiseSuppression: preferences.noiseSuppression !== false,
  };
}

export function loadCapturePreferences(): CapturePreferences {
  try {
    return normalizeCapturePreferences(JSON.parse(localStorage.getItem(CAPTURE_STORAGE_KEY) ?? 'null'));
  } catch {
    return { ...DEFAULT_CAPTURE_PREFERENCES };
  }
}

export function saveCapturePreferences(preferences: CapturePreferences): void {
  try { localStorage.setItem(CAPTURE_STORAGE_KEY, JSON.stringify(normalizeCapturePreferences(preferences))); } catch { /* Storage may be unavailable. */ }
}

/** The preview and publisher use the same requested capture settings. */
export function captureConstraints(preferences: CapturePreferences, kind: 'audio' | 'video'): MediaTrackConstraints {
  if (kind === 'audio') {
    return {
      ...(preferences.microphoneDeviceId && { deviceId: { exact: preferences.microphoneDeviceId } }),
      echoCancellation: preferences.echoCancellation,
      autoGainControl: preferences.autoGainControl,
      noiseSuppression: preferences.noiseSuppression,
    };
  }
  const [width, height] = preferences.resolution === '360p' ? [640, 360]
    : preferences.resolution === '1080p' ? [1920, 1080] : [1280, 720];
  return {
    ...(preferences.cameraDeviceId && { deviceId: { exact: preferences.cameraDeviceId } }),
    width: { ideal: width }, height: { ideal: height }, frameRate: { ideal: preferences.frameRate },
  };
}

export class MediaManager {
  private signaling: SignalingClient;
  private lifecycle = 0;
  private closed = false;
  private device: mediasoupClient.Device | null = null;
  private sendTransport: mediasoupClient.types.Transport | null = null;
  private recvTransport: mediasoupClient.types.Transport | null = null;
  private audioProducer: mediasoupClient.types.Producer | null = null;
  private audioVersion = 0;
  private audioRequested = false;
  private audioActivation: Promise<void> = Promise.resolve();
  private pendingAudioTrack: MediaStreamTrack | null = null;
  private capturePreferences = loadCapturePreferences();
  private videoProducer: mediasoupClient.types.Producer | null = null;
  private videoVersion = 0;
  private videoStarting = false;
  private pendingVideoTrack: MediaStreamTrack | null = null;
  private screenProducer: mediasoupClient.types.Producer | null = null;
  private screenAudioProducer: mediasoupClient.types.Producer | null = null;
  private screenVersion = 0;
  private screenStarting = false;
  private pendingScreenStream: MediaStream | null = null;
  private consumers = new Map<string, mediasoupClient.types.Consumer>();
  // Map producerId → consumerId for cleanup when producer closes
  private producerToConsumer = new Map<string, string>();
  private localStream: MediaStream | null = null;
  // ICE restart timers per transport
  private iceRestartTimers = new Map<string, ReturnType<typeof setTimeout>>();
  // Callback when screen share stops (browser "Stop sharing" or explicit stop)
  private onScreenShareStoppedCb: (() => void) | null = null;
  private onLocalCaptureStoppedCb: ((kind: 'audio' | 'video') => void) | null = null;

  constructor(signaling: SignalingClient) {
    this.signaling = signaling;
  }

  /** Register callback for when screen share stops */
  set onScreenShareStopped(cb: (() => void) | null) {
    this.onScreenShareStoppedCb = cb;
  }

  /** Report external capture termination without automatically reopening a device. */
  set onLocalCaptureStopped(cb: ((kind: 'audio' | 'video') => void) | null) {
    this.onLocalCaptureStoppedCb = cb;
  }

  get audioEnabled(): boolean {
    return this.audioProducer?.closed === false && !this.audioProducer.paused
      && this.audioProducer.track?.readyState === 'live';
  }

  get videoEnabled(): boolean {
    return this.videoProducer?.closed === false && !this.videoProducer.paused
      && this.videoProducer.track?.readyState === 'live';
  }

  private localCaptureStopped(kind: 'audio' | 'video'): void {
    if (this.closed) return;
    const producer = kind === 'audio' ? this.audioProducer : this.videoProducer;
    if (producer) {
      this.closeLocalProducer(producer.id);
      this.signaling.send({ type: 'closeProducer', producerId: producer.id });
    } else if (kind === 'audio') {
      this.cancelAudioActivation();
      this.stopLocalAudioTrack();
    } else {
      this.videoVersion++;
      this.videoStarting = false;
      this.pendingVideoTrack?.stop();
      this.stopLocalVideoTrack();
    }
    this.onLocalCaptureStoppedCb?.(kind);
  }

  /** A track can end before produce/replaceTrack has finished adopting it. */
  private watchPendingCapture(track: MediaStreamTrack, kind: 'audio' | 'video', isCurrent: () => boolean): () => void {
    const onEnded = () => {
      if (isCurrent()) this.localCaptureStopped(kind);
    };
    track.addEventListener('ended', onEnded);
    if (track.readyState === 'ended') onEnded();
    return () => track.removeEventListener('ended', onEnded);
  }

  private watchLocalProducer(producer: mediasoupClient.types.Producer, kind: 'audio' | 'video'): void {
    // mediasoup follows the current track across replacements. Ignore retired
    // producers and intentionally paused capture; track.stop() itself is silent.
    producer.on('trackended', () => {
      const current = kind === 'audio' ? this.audioProducer : this.videoProducer;
      if (current === producer && !producer.paused) this.localCaptureStopped(kind);
    });
  }

  /** Configure future capture without starting a camera or microphone. */
  setCapturePreferences(preferences: CapturePreferences): void {
    this.capturePreferences = normalizeCapturePreferences(preferences);
    saveCapturePreferences(this.capturePreferences);
  }

  /** Load device, create transports */
  async setup(): Promise<void> {
    const generation = this.lifecycle;
    const assertCurrent = () => {
      if (this.closed || generation !== this.lifecycle) throw new Error('Media session closed');
    };
    assertCurrent();
    // 1. Get router RTP capabilities
    const capsResponse = await this.signaling.request<
      Extract<ServerMessage, { type: 'routerRtpCapabilities' }>
    >({ type: 'getRouterRtpCapabilities' }, 'routerRtpCapabilities');
    assertCurrent();

    // 2. Load device
    const device = new mediasoupClient.Device();
    this.device = device;
    await device.load({ routerRtpCapabilities: capsResponse.rtpCapabilities });
    assertCurrent();
    console.log('[media] device loaded');

    // 3. Create send transport
    const sendResponse = await this.signaling.request<
      Extract<ServerMessage, { type: 'transportCreated' }>
    >({ type: 'createSendTransport' }, 'transportCreated');
    assertCurrent();

    const sendTransport = device.createSendTransport({
      id: sendResponse.transportId,
      iceParameters: sendResponse.iceParameters,
      iceCandidates: sendResponse.iceCandidates,
      dtlsParameters: sendResponse.dtlsParameters,
      iceServers: sendResponse.iceServers,
    });
    this.sendTransport = sendTransport;

    sendTransport.on('connect', ({ dtlsParameters }, callback, errback) => {
      try { assertCurrent(); } catch (error) { errback(error as Error); return; }
      this.signaling
        .request(
          { type: 'connectTransport', transportId: sendTransport.id, dtlsParameters },
          'transportConnected',
        )
        .then(() => { assertCurrent(); callback(); })
        .catch(errback);
    });

    sendTransport.on('produce', async ({ kind, rtpParameters, appData }, callback, errback) => {
      try {
        assertCurrent();
        const resp = await this.signaling.request<
          Extract<ServerMessage, { type: 'producerCreated' }>
        >(
          { type: 'produce', transportId: sendTransport.id, kind, rtpParameters, source: appData?.source as string | undefined },
          'producerCreated',
        );
        assertCurrent();
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
    assertCurrent();

    const recvTransport = device.createRecvTransport({
      id: recvResponse.transportId,
      iceParameters: recvResponse.iceParameters,
      iceCandidates: recvResponse.iceCandidates,
      dtlsParameters: recvResponse.dtlsParameters,
      iceServers: recvResponse.iceServers,
    });
    this.recvTransport = recvTransport;

    recvTransport.on('connect', ({ dtlsParameters }, callback, errback) => {
      try { assertCurrent(); } catch (error) { errback(error as Error); return; }
      this.signaling
        .request(
          { type: 'connectTransport', transportId: recvTransport.id, dtlsParameters },
          'transportConnected',
        )
        .then(() => { assertCurrent(); callback(); })
        .catch(errback);
    });

    this.setupIceRecovery(this.recvTransport);

    console.log('[media] recv transport created:', this.recvTransport.id);
  }

  /** Monitor transport connection state and request ICE restart on failure */
  private setupIceRecovery(transport: mediasoupClient.types.Transport): void {
    transport.on('connectionstatechange', (state: string) => {
      if (this.closed || (this.sendTransport !== transport && this.recvTransport !== transport)) return;
      console.log(`[media] transport ${transport.id} connection state: ${state}`);

      if (state === 'disconnected') {
        // Wait 3 seconds before requesting ICE restart
        if (!this.iceRestartTimers.has(transport.id)) {
          const timer = setTimeout(() => {
            this.iceRestartTimers.delete(transport.id);
            if (this.closed || (this.sendTransport !== transport && this.recvTransport !== transport)) return;
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

  /** Serialize capture and discard results after mute, leave, or producer revocation. */
  private async activateAudio(version: number): Promise<void> {
    const transport = this.sendTransport;
    const isCurrent = () => version === this.audioVersion && transport === this.sendTransport;
    if (!transport || !isCurrent() || this.audioEnabled) return;

    let track: MediaStreamTrack | undefined;
    let unwatchCapture: (() => void) | undefined;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        audio: captureConstraints(this.capturePreferences, 'audio'),
      });
      track = stream.getAudioTracks()[0];
      if (!track) throw new Error('No microphone track was available');
      // A produce/replaceTrack operation can outlive a PTT release. Keep its
      // track silent until the operation completes and the intent is rechecked.
      track.enabled = false;
      if (!isCurrent()) return;
      this.pendingAudioTrack = track;
      unwatchCapture = this.watchPendingCapture(track, 'audio', isCurrent);
      if (!isCurrent()) return;

      const existing = this.audioProducer;
      if (existing) {
        await existing.replaceTrack({ track });
        if (!isCurrent() || this.audioProducer !== existing) return;
        if (track.readyState === 'ended') {
          this.localCaptureStopped('audio');
          return;
        }
        existing.resume();
        this.signaling.send({ type: 'resumeProducer', producerId: existing.id });
      } else {
        const producer = await transport.produce({ track, appData: { source: 'microphone' } });
        if (!isCurrent() || track.readyState === 'ended') {
          producer.close();
          this.signaling.send({ type: 'closeProducer', producerId: producer.id });
          if (isCurrent()) this.localCaptureStopped('audio');
          return;
        }
        producer.resume();
        this.audioProducer = producer;
        this.watchLocalProducer(producer, 'audio');
      }

      this.stopLocalAudioTrack();
      if (!this.localStream) this.localStream = new MediaStream();
      this.localStream.addTrack(track);
      track.enabled = true;
      track = undefined; // Ownership transferred to the producer/local stream.
    } catch (error) {
      if (isCurrent()) this.audioRequested = false;
      throw error;
    } finally {
      unwatchCapture?.();
      track?.stop();
      this.pendingAudioTrack = null;
    }
  }

  /** Lazily capture camera and create video producer with simulcast */
  private async ensureVideoProducer(): Promise<boolean> {
    if (this.videoProducer) return true;
    const transport = this.sendTransport;
    if (!transport || this.videoStarting) return false;
    const version = ++this.videoVersion;
    this.videoStarting = true;
    const isCurrent = () => version === this.videoVersion && transport === this.sendTransport;
    let videoTrack: MediaStreamTrack | undefined;
    let unwatchCapture: (() => void) | undefined;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        video: captureConstraints(this.capturePreferences, 'video'),
      });
      videoTrack = stream.getVideoTracks()[0];
      if (!videoTrack || !isCurrent()) return false;
      this.pendingVideoTrack = videoTrack;
      unwatchCapture = this.watchPendingCapture(videoTrack, 'video', isCurrent);
      if (!isCurrent()) return false;
      const producer = await transport.produce({
        track: videoTrack,
        encodings: [
          { rid: 'r0', maxBitrate: 100_000, scaleResolutionDownBy: 4 },
          { rid: 'r1', maxBitrate: 300_000, scaleResolutionDownBy: 2 },
          { rid: 'r2', maxBitrate: this.capturePreferences.resolution === '1080p' ? 2_500_000 : 900_000 },
        ],
        codecOptions: { videoGoogleStartBitrate: 1000 },
        appData: { source: 'camera' },
      });
      if (!isCurrent() || videoTrack.readyState === 'ended') {
        producer.close();
        this.signaling.send({ type: 'closeProducer', producerId: producer.id });
        if (isCurrent()) this.localCaptureStopped('video');
        return false;
      }
      this.videoProducer = producer;
      this.watchLocalProducer(producer, 'video');
      if (!this.localStream) this.localStream = new MediaStream();
      this.localStream.addTrack(videoTrack);
      videoTrack = undefined;
      return true;
    } finally {
      unwatchCapture?.();
      if (this.pendingVideoTrack === videoTrack) this.pendingVideoTrack = null;
      videoTrack?.stop();
      if (isCurrent()) {
        this.videoStarting = false;
        this.pendingVideoTrack = null;
      }
    }
  }

  /** Consume a remote producer, returns the track */
  async consume(producerId: string): Promise<MediaStreamTrack> {
    const device = this.device;
    const transport = this.recvTransport;
    const generation = this.lifecycle;
    if (this.closed || !device || !transport) {
      throw new Error('Device/transport not ready');
    }
    const isCurrent = () => !this.closed && generation === this.lifecycle && this.device === device && this.recvTransport === transport;
    const assertCurrent = () => { if (!isCurrent()) throw new Error('Media session closed'); };

    const response = await this.signaling.request<
      Extract<ServerMessage, { type: 'consumerCreated' }>
    >(
      { type: 'consume', producerId, rtpCapabilities: device.rtpCapabilities },
      'consumerCreated',
    );
    assertCurrent();

    const consumer = await transport.consume({
      id: response.consumerId,
      producerId: response.producerId,
      kind: response.kind,
      rtpParameters: response.rtpParameters,
    });
    try {
      assertCurrent();
      this.consumers.set(response.consumerId, consumer);
      this.producerToConsumer.set(producerId, response.consumerId);
      // Resume only after the receiver exists and this session is still active.
      await this.signaling.request(
        { type: 'resumeConsumer', consumerId: response.consumerId },
        'consumerResumed',
      );
      assertCurrent();
      console.log(`[media] consuming ${response.kind} from producer ${producerId}`);
      return consumer.track;
    } catch (error) {
      consumer.close();
      if (this.consumers.get(response.consumerId) === consumer) this.consumers.delete(response.consumerId);
      if (this.producerToConsumer.get(producerId) === response.consumerId) this.producerToConsumer.delete(producerId);
      if (isCurrent()) this.signaling.send({ type: 'pauseConsumer', consumerId: response.consumerId });
      throw error;
    }
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

  /** Pause only this viewer's consumer; the publisher and other viewers are unaffected. */
  setConsumerHiddenByProducer(producerId: string, hidden: boolean): void {
    const consumerId = this.producerToConsumer.get(producerId);
    const consumer = consumerId ? this.consumers.get(consumerId) : undefined;
    if (!consumer || consumer.closed || consumer.paused === hidden) return;
    if (hidden) consumer.pause();
    else consumer.resume();
    this.signaling.send({ type: hidden ? 'pauseConsumer' : 'resumeConsumer', consumerId: consumer.id });
  }

  /** Cap simulcast quality when layers exist. Auto restores the highest available cap. */
  setConsumerQualityByProducer(producerId: string, quality: RemoteVideoQuality): boolean {
    const consumerId = this.producerToConsumer.get(producerId);
    const consumer = consumerId ? this.consumers.get(consumerId) : undefined;
    if (!consumer || consumer.closed || consumer.kind !== 'video') return false;
    const spatialLayers = Math.max(1, ...(consumer.rtpParameters.encodings ?? []).map(encoding => {
      const match = /^[LS](\d+)T/.exec(encoding.scalabilityMode ?? '');
      return match ? Number(match[1]) : 1;
    }));
    if (spatialLayers <= 1) return false;
    const requested = quality === 'low' ? 0 : quality === 'medium' ? 1 : spatialLayers - 1;
    this.setPreferredLayers(consumer.id, Math.min(requested, spatialLayers - 1));
    return true;
  }

  /** Get a consumer's track by its associated producer ID */
  getConsumerTrackByProducer(producerId: string): MediaStreamTrack | null {
    const consumerId = this.producerToConsumer.get(producerId);
    if (!consumerId) return null;
    const consumer = this.consumers.get(consumerId);
    return consumer?.track ?? null;
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

  /** Release a producer revoked by the server so it can be created again later. */
  closeLocalProducer(producerId: string): boolean {
    if (this.audioProducer?.id === producerId) {
      this.cancelAudioActivation();
      const producer = this.audioProducer;
      this.audioProducer = null;
      producer.track?.stop();
      producer.close();
      this.stopLocalAudioTrack();
      return true;
    }
    if (this.videoProducer?.id === producerId) {
      this.videoVersion++;
      this.videoStarting = false;
      const producer = this.videoProducer;
      this.videoProducer = null;
      this.pendingVideoTrack?.stop();
      producer.track?.stop();
      producer.close();
      this.stopLocalVideoTrack();
      return true;
    }
    if (this.screenProducer?.id === producerId || this.screenAudioProducer?.id === producerId) {
      // Screen video and its optional audio share one capture session/control.
      this.stopScreenShare(producerId);
      return true;
    }
    return false;
  }

  /** Release local capture for server-side closures missed during a disconnected interval. */
  reconcileLocalProducers(producerIds: readonly string[]): boolean {
    const active = new Set(producerIds);
    const localIds = [this.audioProducer?.id, this.videoProducer?.id, this.screenProducer?.id, this.screenAudioProducer?.id];
    let changed = false;
    for (const id of localIds) {
      if (id && !active.has(id)) changed = this.closeLocalProducer(id) || changed;
    }
    return changed;
  }

  getLocalStream(): MediaStream | null {
    return this.localStream;
  }

  /** Stop and release a local audio track */
  private stopLocalAudioTrack(): void {
    const track = this.localStream?.getAudioTracks()[0];
    if (track) {
      track.stop();
      this.localStream?.removeTrack(track);
    }
  }

  /** Stop and release a local video track (turns off camera light) */
  private stopLocalVideoTrack(): void {
    const track = this.localStream?.getVideoTracks()[0];
    if (track) {
      track.stop();
      this.localStream?.removeTrack(track);
    }
  }

  /** Re-capture camera and replace track on existing producer */
  private async recaptureVideo(deviceId?: string): Promise<boolean> {
    const producer = this.videoProducer;
    if (!producer) return false;
    const version = ++this.videoVersion;
    const isCurrent = () => this.videoProducer === producer && version === this.videoVersion;
    let track: MediaStreamTrack | undefined;
    let unwatchCapture: (() => void) | undefined;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({
        video: {
          ...captureConstraints(this.capturePreferences, 'video'),
          ...(deviceId && { deviceId: { exact: deviceId } }),
        },
      });
      track = stream.getVideoTracks()[0];
      if (!track || !isCurrent()) return false;
      this.pendingVideoTrack = track;
      unwatchCapture = this.watchPendingCapture(track, 'video', isCurrent);
      if (!isCurrent()) return false;
      await producer.replaceTrack({ track });
      if (!isCurrent()) return false;
      if (track.readyState === 'ended') {
        this.localCaptureStopped('video');
        return false;
      }
      this.stopLocalVideoTrack();
      if (!this.localStream) this.localStream = new MediaStream();
      this.localStream.addTrack(track);
      track = undefined;
      return true;
    } finally {
      unwatchCapture?.();
      if (this.pendingVideoTrack === track || isCurrent()) this.pendingVideoTrack = null;
      track?.stop();
    }
  }

  /** Toggle local audio — lazily captures mic on first call */
  async toggleAudio(): Promise<boolean> {
    if (this.audioRequested || this.audioEnabled) {
      this.muteAudio();
    } else {
      await this.unmuteAudio();
    }
    return this.audioEnabled;
  }

  /** Explicitly mute audio — stops mic track */
  muteAudio(): void {
    this.cancelAudioActivation();
    if (this.audioProducer && !this.audioProducer.paused) {
      this.audioProducer.pause();
      this.signaling.send({ type: 'pauseProducer', producerId: this.audioProducer.id });
    }
    this.stopLocalAudioTrack();
  }

  /** Explicitly unmute audio — re-captures mic, lazily creates producer on first call */
  unmuteAudio(): Promise<void> {
    this.audioRequested = true;
    const version = ++this.audioVersion;
    const activation = this.audioActivation.catch(() => {}).then(() => this.activateAudio(version));
    this.audioActivation = activation;
    return activation;
  }

  private cancelAudioActivation(): void {
    this.audioVersion++;
    this.audioRequested = false;
    this.pendingAudioTrack?.stop();
  }

  /** Explicitly pause video — stops camera track */
  pauseVideo(): void {
    this.videoVersion++;
    this.videoStarting = false;
    this.pendingVideoTrack?.stop();
    if (this.videoProducer && !this.videoProducer.paused) {
      this.videoProducer.pause();
      this.signaling.send({ type: 'pauseProducer', producerId: this.videoProducer.id });
      this.stopLocalVideoTrack();
    }
  }

  /** Explicitly unpause video — re-captures camera, lazily creates producer on first call */
  async unmuteVideo(): Promise<void> {
    const producer = this.videoProducer;
    if (!producer) {
      await this.ensureVideoProducer();
      return;
    }
    if (producer.paused) {
      if (!(await this.recaptureVideo()) || this.videoProducer !== producer) return;
      producer.resume();
      this.signaling.send({ type: 'resumeProducer', producerId: producer.id });
    }
  }

  /** Toggle local video — lazily captures camera on first call */
  async toggleVideo(): Promise<boolean> {
    if (!this.videoProducer || this.videoProducer.paused) {
      await this.unmuteVideo();
    } else {
      this.pauseVideo();
    }
    return this.videoEnabled;
  }

  /** Switch to a different camera device */
  async switchCamera(deviceId: string): Promise<void> {
    this.setCapturePreferences({ ...this.capturePreferences, cameraDeviceId: deviceId });
    if (this.videoEnabled) await this.recaptureVideo(deviceId);
  }

  /** Switch to a different microphone device */
  async switchMic(deviceId: string): Promise<void> {
    this.setCapturePreferences({ ...this.capturePreferences, microphoneDeviceId: deviceId });
    if (!this.audioRequested && !this.audioEnabled) return;
    this.muteAudio();
    await this.unmuteAudio();
  }

  /** Start screen sharing — creates screen video producer (and optional audio) */
  async startScreenShare(): Promise<{ videoTrack: MediaStreamTrack; audioTrack?: MediaStreamTrack } | null> {
    const transport = this.sendTransport;
    if (!transport || this.screenProducer || this.screenStarting) return null;
    const version = ++this.screenVersion;
    this.screenStarting = true;
    const isCurrent = () => version === this.screenVersion && transport === this.sendTransport;
    let stream: MediaStream | undefined;
    let started = false;
    try {
      try {
        stream = await navigator.mediaDevices.getDisplayMedia({ video: true, audio: true });
      } catch {
        return null; // Picker cancellation or permission denial.
      }
      if (!isCurrent()) return null;
      this.pendingScreenStream = stream;
      const videoTrack = stream.getVideoTracks()[0];
      if (!videoTrack) return null;

      const videoProducer = await transport.produce({
        track: videoTrack,
        appData: { source: 'screen' },
      });
      if (!isCurrent()) {
        videoProducer.close();
        this.signaling.send({ type: 'closeProducer', producerId: videoProducer.id });
        return null;
      }
      this.screenProducer = videoProducer;
      videoTrack.addEventListener('ended', () => {
        if (this.screenProducer === videoProducer) this.stopScreenShare();
      });

      const audioTrack = stream.getAudioTracks()[0];
      if (audioTrack) {
        const audioProducer = await transport.produce({
          track: audioTrack,
          appData: { source: 'screen-audio' },
        });
        if (!isCurrent()) {
          audioProducer.close();
          this.signaling.send({ type: 'closeProducer', producerId: audioProducer.id });
          return null;
        }
        this.screenAudioProducer = audioProducer;
      }
      started = true;
      return { videoTrack, audioTrack };
    } catch (error) {
      if (!isCurrent()) return null;
      this.stopScreenShare();
      throw error;
    } finally {
      if (!started) stream?.getTracks().forEach(track => track.stop());
      if (isCurrent()) {
        this.screenStarting = false;
        this.pendingScreenStream = null;
      }
    }
  }

  /** Stop screen sharing — closes producers and notifies server.
   *  Uses null-then-act pattern to prevent duplicate closeProducer messages
   *  when browser "Stop sharing" and user click race. */
  stopScreenShare(revokedProducerId?: string): void {
    this.cancelScreenStart();
    const sp = this.screenProducer;
    const sap = this.screenAudioProducer;
    this.screenProducer = null;
    this.screenAudioProducer = null;

    if (sp) {
      const track = sp.track;
      if (track) track.stop();
      sp.close();
      if (sp.id !== revokedProducerId) {
        this.signaling.send({ type: 'closeProducer', producerId: sp.id });
      }
    }
    if (sap) {
      const track = sap.track;
      if (track) track.stop();
      sap.close();
      if (sap.id !== revokedProducerId) {
        this.signaling.send({ type: 'closeProducer', producerId: sap.id });
      }
    }
    this.onScreenShareStoppedCb?.();
  }

  private cancelScreenStart(): void {
    this.screenVersion++;
    this.screenStarting = false;
    this.pendingScreenStream?.getTracks().forEach(track => track.stop());
    this.pendingScreenStream = null;
  }

  get isScreenSharing(): boolean {
    return this.screenProducer !== null && !this.screenProducer.closed;
  }

  close(): void {
    this.closed = true;
    this.lifecycle++;
    this.cancelAudioActivation();
    this.videoVersion++;
    this.videoStarting = false;
    this.pendingVideoTrack?.stop();
    this.cancelScreenStart();
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
    this.screenProducer?.close();
    this.screenAudioProducer?.close();
    this.audioProducer = null;
    this.videoProducer = null;
    this.screenProducer = null;
    this.screenAudioProducer = null;

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
