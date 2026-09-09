import {
  captureConstraints, loadCapturePreferences, normalizeCapturePreferences, saveCapturePreferences,
  type CapturePreferences, type RemoteVideoQuality,
} from './media';
import './media-controls.css';

interface MediaControlsRoom {
  setCapturePreferences(preferences: CapturePreferences): void;
  setRemoteMediaHidden(participantId: string, hidden: boolean): void;
  setRemoteVideoQuality(participantId: string, quality: RemoteVideoQuality): void;
}

interface MediaControlsOptions {
  getRoom: () => MediaControlsRoom | null;
  notify: (message: string) => void;
}

interface PersonalPlaybackPreferences {
  volume: number;
  muted: boolean;
  hidden: boolean;
  quality: RemoteVideoQuality;
}

const MASTER_VOLUME_KEY = 'simplestchat.masterVolume';
const SETUP_KEY = 'simplestchat.mediaSetupConfigured';

function readStored(key: string): string | null {
  try { return localStorage.getItem(key); } catch { return null; }
}

function writeStored(key: string, value: string): void {
  try { localStorage.setItem(key, value); } catch { /* Preferences still work for this session. */ }
}

function volumeValue(value: number): number {
  return Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : 1;
}

/** Adjust this browser's playback only; never mutate a publisher's track. */
export function applyPersonalPlayback(
  elements: Iterable<HTMLMediaElement>, preferences: Pick<PersonalPlaybackPreferences, 'volume' | 'muted' | 'hidden'>,
  masterVolume: number,
): void {
  for (const element of elements) {
    element.volume = volumeValue(preferences.volume) * volumeValue(masterVolume);
    element.muted = preferences.muted || preferences.hidden;
    if (preferences.hidden) element.pause();
    else if (element.paused && element.srcObject) {
      // Browsers may require a separate click to allow playback; media controls remain available.
      void element.play().catch(() => {});
    }
  }
}

/** Owns a preview capture independently from room producers. Closing invalidates late permissions. */
export class MediaPreview {
  private generation = 0;
  private stream: MediaStream | null = null;
  private audioContext: AudioContext | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private frame: number | null = null;

  constructor(
    private onStream: (stream: MediaStream | null) => void,
    private onLevel: (level: number) => void,
  ) {}

  async start(preferences: CapturePreferences, video = true, audio = true): Promise<void> {
    this.stop();
    const generation = this.generation;
    if (!video && !audio) throw new Error('Select a camera or microphone to preview.');
    try {
      if (!navigator.mediaDevices?.getUserMedia) throw new Error('Media preview requires localhost or a secure connection.');
      const stream = await navigator.mediaDevices.getUserMedia({
        video: video ? captureConstraints(preferences, 'video') : false,
        audio: audio ? captureConstraints(preferences, 'audio') : false,
      });
      if (generation !== this.generation) {
        stream.getTracks().forEach(track => track.stop());
        return;
      }
      this.stream = stream;
      this.onStream(stream);
      if (stream.getAudioTracks().length && typeof window.AudioContext === 'function') {
        try {
          const context = new AudioContext();
          this.audioContext = context;
          const analyser = context.createAnalyser();
          analyser.fftSize = 512;
          this.source = context.createMediaStreamSource(stream);
          this.source.connect(analyser);
          // The analyser is deliberately not connected to speakers (no microphone feedback).
          void context.resume().catch(() => {});
          const samples = new Uint8Array(analyser.fftSize);
          const update = () => {
            if (generation !== this.generation) return;
            analyser.getByteTimeDomainData(samples);
            const energy = samples.reduce((sum, sample) => sum + ((sample - 128) / 128) ** 2, 0);
            this.onLevel(Math.min(1, Math.sqrt(energy / samples.length) * 4));
            this.frame = requestAnimationFrame(update);
          };
          update();
        } catch {
          // Capture remains usable if this browser cannot create an audio meter.
          this.source?.disconnect();
          this.source = null;
          void this.audioContext?.close().catch(() => {});
          this.audioContext = null;
        }
      }
    } catch (error) {
      if (generation !== this.generation) return;
      this.stop();
      throw error;
    }
  }

  stop(): void {
    this.generation++;
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.frame = null;
    this.source?.disconnect();
    this.source = null;
    void this.audioContext?.close().catch(() => {});
    this.audioContext = null;
    this.stream?.getTracks().forEach(track => track.stop());
    this.stream = null;
    this.onStream(null);
    this.onLevel(0);
  }
}

export class MediaControls {
  private tiles = new Map<HTMLElement, { participantId: string; controls: HTMLDetailsElement }>();
  private playback = new Map<string, PersonalPlaybackPreferences>();
  private toolbars = new Set<HTMLElement>();
  private preview: MediaPreview | null = null;
  private dialog: HTMLDialogElement | null = null;
  private finishSetup: ((saved: boolean) => void) | null = null;
  private configured = readStored(SETUP_KEY) === 'true';
  private masterVolume = volumeValue(Number(readStored(MASTER_VOLUME_KEY) ?? 1));

  constructor(private options: MediaControlsOptions) {}

  get hasConfiguredSetup(): boolean { return this.configured; }

  mountToolbar(container: HTMLElement): void {
    const toolbar = document.createElement('div');
    toolbar.className = 'media-controls-toolbar';
    const setup = this.button('Media setup', () => { void this.openSetup('settings'); });
    const label = document.createElement('label');
    label.textContent = 'Room volume';
    const slider = document.createElement('input');
    slider.type = 'range';
    slider.min = '0'; slider.max = '100'; slider.step = '1';
    slider.value = String(Math.round(this.masterVolume * 100));
    slider.setAttribute('aria-label', 'Room volume');
    slider.title = `Room volume: ${slider.value}%`;
    slider.addEventListener('input', () => {
      this.masterVolume = volumeValue(Number(slider.value) / 100);
      writeStored(MASTER_VOLUME_KEY, String(this.masterVolume));
      for (const mounted of this.toolbars) {
        const input = mounted.querySelector('input');
        if (input) {
          input.value = slider.value;
          input.title = `Room volume: ${slider.value}%`;
        }
      }
      for (const participantId of this.playback.keys()) this.applyParticipant(participantId);
    });
    label.append(slider);
    toolbar.append(setup, label);
    container.append(toolbar);
    this.toolbars.add(toolbar);
  }

  /** Opens only on user action. Saving configures later capture; it never starts a broadcast. */
  openSetup(mode: 'camera' | 'microphone' | 'settings' = 'settings'): Promise<boolean> {
    this.closeSetup(false);
    const preferences = loadCapturePreferences();
    const dialog = document.createElement('dialog');
    dialog.className = 'media-setup-dialog';
    dialog.setAttribute('aria-labelledby', 'media-setup-title');
    dialog.innerHTML = `
      <form method="dialog" class="media-setup-form">
        <div class="media-setup-heading"><h2 id="media-setup-title">Camera &amp; microphone</h2>
          <button type="button" data-action="cancel" aria-label="Close media setup">Close</button></div>
        <video class="media-preview-video" autoplay muted playsinline hidden></video>
        <p class="media-preview-status" role="status">Preview your devices before broadcasting.</p>
        <div class="media-preview-options">
          <label><input type="checkbox" name="previewCamera"> Camera preview</label>
          <label><input type="checkbox" name="previewMicrophone" checked> Microphone preview</label>
        </div>
        <label class="media-meter-label">Microphone level <meter min="0" max="1" value="0" aria-label="Microphone input level"></meter></label>
        <div class="media-setup-fields">
          <label>Camera <select name="cameraDeviceId"><option value="">Default camera</option></select></label>
          <label>Microphone <select name="microphoneDeviceId"><option value="">Default microphone</option></select></label>
          <label>Resolution <select name="resolution"><option value="360p">360p — less bandwidth</option><option value="720p">720p — balanced</option><option value="1080p">1080p — sharper video</option></select></label>
          <label>Frame rate <select name="frameRate"><option value="15">15 fps</option><option value="30">30 fps</option><option value="60">60 fps</option></select></label>
        </div>
        <fieldset class="media-audio-options"><legend>Microphone processing</legend>
          <label><input type="checkbox" name="echoCancellation"> Echo cancellation</label>
          <label><input type="checkbox" name="noiseSuppression"> Noise suppression</label>
          <label><input type="checkbox" name="autoGainControl"> Automatic level</label>
        </fieldset>
        <p class="media-setup-hint">Saved settings apply when you next enable your camera or microphone.</p>
        <div class="media-setup-actions"><button type="button" data-action="preview">Start preview</button>
          <button type="button" data-action="stop">Stop preview</button>
          <button type="submit" value="save">Save settings</button></div>
      </form>`;
    const field = <T extends HTMLInputElement | HTMLSelectElement>(name: string) => dialog.querySelector<T>(`[name="${name}"]`)!;
    field<HTMLInputElement>('previewCamera').checked = mode !== 'microphone';
    for (const name of ['cameraDeviceId', 'microphoneDeviceId'] as const) {
      if (preferences[name]) field<HTMLSelectElement>(name).add(new Option('Saved device', preferences[name]));
      field<HTMLSelectElement>(name).value = preferences[name];
    }
    for (const name of ['resolution', 'frameRate'] as const) field<HTMLSelectElement>(name).value = String(preferences[name]);
    for (const name of ['echoCancellation', 'noiseSuppression', 'autoGainControl'] as const) field<HTMLInputElement>(name).checked = preferences[name];
    const status = dialog.querySelector<HTMLElement>('.media-preview-status')!;
    const video = dialog.querySelector('video')!;
    video.muted = true;
    const meter = dialog.querySelector('meter')!;
    const readPreferences = (): CapturePreferences => normalizeCapturePreferences({
      cameraDeviceId: field<HTMLSelectElement>('cameraDeviceId').value,
      microphoneDeviceId: field<HTMLSelectElement>('microphoneDeviceId').value,
      resolution: field<HTMLSelectElement>('resolution').value,
      frameRate: Number(field<HTMLSelectElement>('frameRate').value),
      echoCancellation: field<HTMLInputElement>('echoCancellation').checked,
      noiseSuppression: field<HTMLInputElement>('noiseSuppression').checked,
      autoGainControl: field<HTMLInputElement>('autoGainControl').checked,
    });
    const fillDevices = async () => {
      try {
        const devices = await navigator.mediaDevices?.enumerateDevices() ?? [];
        if (this.dialog !== dialog) return;
        for (const [name, kind, title] of [
          ['cameraDeviceId', 'videoinput', 'camera'], ['microphoneDeviceId', 'audioinput', 'microphone'],
        ] as const) {
          const select = field<HTMLSelectElement>(name);
          const selected = select.value;
          select.replaceChildren(new Option(`Default ${title}`, ''));
          const matching = devices.filter(device => device.kind === kind && device.deviceId);
          for (const [index, device] of matching.entries()) select.add(new Option(device.label || `${title} ${index + 1}`, device.deviceId));
          if (selected && !matching.some(device => device.deviceId === selected)) {
            // Device names/IDs may be concealed until the user grants preview permission.
            select.add(new Option(`Saved ${title} (not currently listed)`, selected));
          }
          select.value = selected;
        }
      } catch {
        if (this.dialog === dialog) status.textContent = 'Device list unavailable. You can still try the default devices.';
      }
    };
    const preview = new MediaPreview(stream => {
      video.srcObject = stream;
      video.hidden = !stream?.getVideoTracks().length;
      if (stream) void video.play().catch(() => {});
    }, level => { meter.value = level; });
    this.preview = preview;
    this.dialog = dialog;
    const previewButton = dialog.querySelector<HTMLButtonElement>('[data-action="preview"]')!;
    let previewAction = 0;
    dialog.querySelector('[data-action="cancel"]')!.addEventListener('click', () => this.closeSetup(false));
    dialog.querySelector('[data-action="stop"]')!.addEventListener('click', () => {
      previewAction++;
      preview.stop();
      previewButton.disabled = false;
      status.textContent = 'Preview stopped.';
    });
    previewButton.addEventListener('click', async () => {
      const action = ++previewAction;
      previewButton.disabled = true;
      status.textContent = 'Waiting for device permission…';
      try {
        await preview.start(readPreferences(), field<HTMLInputElement>('previewCamera').checked, field<HTMLInputElement>('previewMicrophone').checked);
        if (this.dialog !== dialog || action !== previewAction) return;
        status.textContent = 'Preview is local to you. Your microphone is not played through the speakers.';
        await fillDevices();
      } catch (error) {
        if (this.dialog !== dialog || action !== previewAction) return;
        status.textContent = mediaErrorMessage(error);
      } finally {
        if (this.dialog === dialog && action === previewAction) previewButton.disabled = false;
      }
    });
    dialog.querySelector('form')!.addEventListener('change', () => {
      previewAction++;
      preview.stop();
      status.textContent = 'Settings changed. Start preview to try them.';
      previewButton.disabled = false;
    });
    dialog.querySelector('form')!.addEventListener('submit', event => {
      event.preventDefault();
      const next = readPreferences();
      preview.stop();
      try {
        this.options.getRoom()?.setCapturePreferences(next);
        saveCapturePreferences(next);
        this.configured = true;
        writeStored(SETUP_KEY, 'true');
        this.closeSetup(true);
      } catch (error) {
        status.textContent = mediaErrorMessage(error);
      }
    });
    dialog.addEventListener('cancel', event => { event.preventDefault(); this.closeSetup(false); });
    dialog.addEventListener('close', () => { if (this.dialog === dialog) this.closeSetup(false); });
    document.body.append(dialog);
    const result = new Promise<boolean>(resolve => { this.finishSetup = resolve; });
    try {
      dialog.showModal();
      void fillDevices();
    } catch (error) {
      this.closeSetup(false);
      this.options.notify(mediaErrorMessage(error));
    }
    return result;
  }

  /** Call after each added/replaced remote audio/video element, including screen shares. */
  attachTile(tile: HTMLElement, participantId: string, participantName: string): void {
    // Removed tiles can be replaced when a producer pauses/resumes.
    for (const [element] of this.tiles) if (!element.isConnected) this.tiles.delete(element);
    if (!this.playback.has(participantId)) this.playback.set(participantId, { volume: 1, muted: false, hidden: false, quality: 'auto' });
    if (!this.tiles.has(tile)) {
      const details = document.createElement('details');
      details.className = 'personal-media-controls';
      const summary = document.createElement('summary');
      summary.textContent = 'Controls';
      summary.setAttribute('aria-label', `Viewing controls for ${participantName}`);
      const panel = document.createElement('div');
      panel.className = 'personal-media-panel';
      const volumeLabel = document.createElement('label');
      volumeLabel.textContent = 'Volume';
      const volume = document.createElement('input');
      volume.type = 'range'; volume.min = '0'; volume.max = '100'; volume.step = '1';
      volume.dataset['control'] = 'volume';
      volume.setAttribute('aria-label', `Volume for ${participantName}`);
      volume.addEventListener('input', () => {
        this.playback.get(participantId)!.volume = Number(volume.value) / 100;
        this.applyParticipant(participantId);
      });
      volumeLabel.append(volume);
      const mute = this.button('Mute for me', () => {
        const state = this.playback.get(participantId)!;
        state.muted = !state.muted;
        this.applyParticipant(participantId);
      });
      mute.dataset['control'] = 'mute';
      const hide = this.button('Hide for me', () => this.setHidden(participantId, !this.playback.get(participantId)!.hidden));
      hide.dataset['control'] = 'hide';
      const qualityLabel = document.createElement('label');
      qualityLabel.textContent = 'Video quality';
      const quality = document.createElement('select');
      quality.dataset['control'] = 'quality';
      quality.setAttribute('aria-label', `Video quality for ${participantName}`);
      for (const [value, title] of [['auto', 'Auto'], ['low', 'Low'], ['medium', 'Medium'], ['high', 'High']]) quality.add(new Option(title, value));
      quality.title = 'Limits camera quality when the broadcast offers multiple layers. Screen shares use their original quality.';
      quality.addEventListener('change', () => {
        this.playback.get(participantId)!.quality = quality.value as RemoteVideoQuality;
        this.options.getRoom()?.setRemoteVideoQuality(participantId, quality.value as RemoteVideoQuality);
        this.applyParticipant(participantId);
      });
      qualityLabel.append(quality);
      const fullscreen = this.button('Fullscreen', () => {
        void tile.requestFullscreen().catch(() => this.options.notify('Fullscreen is unavailable for this video.'));
      });
      fullscreen.dataset['control'] = 'fullscreen';
      fullscreen.hidden = typeof tile.requestFullscreen !== 'function';
      const pip = this.button('Picture in picture', () => {
        const video = tile.querySelector('video');
        if (!video || typeof video.requestPictureInPicture !== 'function') return;
        const action = document.pictureInPictureElement === video ? document.exitPictureInPicture() : video.requestPictureInPicture();
        void action.catch(() => this.options.notify('Picture in picture is unavailable for this video.'));
      });
      pip.dataset['control'] = 'pip';
      panel.append(volumeLabel, mute, hide, qualityLabel, fullscreen, pip);
      details.append(summary, panel);
      // Keep viewing controls from opening the tile's moderation context menu.
      details.addEventListener('contextmenu', event => event.stopPropagation());
      const hiddenNotice = document.createElement('div');
      hiddenNotice.className = 'personal-media-hidden-notice';
      hiddenNotice.hidden = true;
      const text = document.createElement('span');
      text.textContent = `${participantName}'s broadcast is hidden for you.`;
      hiddenNotice.append(text, this.button('Restore broadcast', () => this.setHidden(participantId, false)));
      tile.append(details, hiddenNotice);
      this.tiles.set(tile, { participantId, controls: details });
    }
    this.applyParticipant(participantId);
    const state = this.playback.get(participantId)!;
    if (state.hidden) this.options.getRoom()?.setRemoteMediaHidden(participantId, true);
    if (state.quality !== 'auto') this.options.getRoom()?.setRemoteVideoQuality(participantId, state.quality);
  }

  detachParticipant(participantId: string): void {
    for (const [tile, info] of this.tiles) {
      if (info.participantId === participantId) {
        info.controls.remove();
        tile.querySelector('.personal-media-hidden-notice')?.remove();
        this.tiles.delete(tile);
      }
    }
    this.playback.delete(participantId);
  }

  reset(): void {
    this.closeSetup(false);
    for (const id of this.playback.keys()) this.detachParticipant(id);
  }

  destroy(): void {
    this.reset();
    for (const toolbar of this.toolbars) toolbar.remove();
    this.toolbars.clear();
  }

  private closeSetup(saved: boolean): void {
    this.preview?.stop();
    this.preview = null;
    const dialog = this.dialog;
    this.dialog = null;
    if (dialog?.open) dialog.close();
    dialog?.remove();
    const resolve = this.finishSetup;
    this.finishSetup = null;
    resolve?.(saved);
  }

  private setHidden(participantId: string, hidden: boolean): void {
    this.playback.get(participantId)!.hidden = hidden;
    this.applyParticipant(participantId);
    this.options.getRoom()?.setRemoteMediaHidden(participantId, hidden);
  }

  private applyParticipant(participantId: string): void {
    const state = this.playback.get(participantId);
    if (!state) return;
    for (const [tile, info] of this.tiles) {
      if (info.participantId !== participantId) continue;
      applyPersonalPlayback(tile.querySelectorAll<HTMLMediaElement>('video, audio'), state, this.masterVolume);
      tile.classList.toggle('personal-media-hidden', state.hidden);
      const notice = tile.querySelector<HTMLElement>('.personal-media-hidden-notice');
      if (notice) notice.hidden = !state.hidden;
      const volume = info.controls.querySelector<HTMLInputElement>('[data-control="volume"]')!;
      volume.value = String(Math.round(state.volume * 100));
      volume.title = `Volume: ${volume.value}%`;
      const mute = info.controls.querySelector<HTMLButtonElement>('[data-control="mute"]')!;
      mute.textContent = state.muted ? 'Unmute for me' : 'Mute for me';
      mute.setAttribute('aria-pressed', String(state.muted));
      info.controls.querySelector('[data-control="hide"]')!.textContent = state.hidden ? 'Restore broadcast' : 'Hide for me';
      const quality = info.controls.querySelector<HTMLSelectElement>('[data-control="quality"]')!;
      quality.value = state.quality;
      quality.disabled = !tile.querySelector('video') || tile.classList.contains('screen-share') || state.hidden;
      const pip = info.controls.querySelector<HTMLButtonElement>('[data-control="pip"]')!;
      pip.hidden = !document.pictureInPictureEnabled || !tile.querySelector('video');
      pip.disabled = state.hidden;
    }
  }

  private button(text: string, action: () => void): HTMLButtonElement {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = text;
    button.addEventListener('click', action);
    return button;
  }
}

function mediaErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    if (error.name === 'NotAllowedError') return 'Camera or microphone permission was denied. Allow access in your browser and try again.';
    if (error.name === 'NotFoundError') return 'No matching camera or microphone was found. Select another device or preview only one device.';
    if (error.name === 'OverconstrainedError') return 'The selected device is unavailable. Choose a default device and try again.';
    if (error.name === 'NotReadableError') return 'The device could not be opened. Check whether another app is using it.';
    return error.message;
  }
  return 'Could not open your media devices. Please try again.';
}
