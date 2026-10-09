import {
  captureConstraints,
  captureMedia,
  loadCapturePreferences,
  normalizeCapturePreferences,
  saveCapturePreferences,
  type CapturePreferences,
  type RemoteVideoQuality,
} from './media';
import './media-controls.css';
import { configureSettingsDialog } from './settings-dialog';
import './settings-dialog.css';
import { AudioOutput, SpeakerTest, observeMediaDevices, outputErrorMessage } from './audio-output';
import {
  loadReceiveMode,
  normalizeReceiveMode,
  saveReceiveMode,
  type ReceiveMode,
} from './receive-policy';

interface MediaControlsRoom {
  setCapturePreferences(preferences: CapturePreferences): void;
  readonly audioEnabled: boolean;
  readonly videoEnabled: boolean;
  switchCamera(deviceId: string): Promise<void>;
  switchMic(deviceId: string): Promise<void>;
  setRemoteMediaHidden(participantId: string, hidden: boolean): void;
  setRemoteVideoQuality(participantId: string, quality: RemoteVideoQuality): void;
  setReceiveMode(mode: ReceiveMode): void;
}

interface MediaControlsOptions {
  getRoom: () => MediaControlsRoom | null;
  notify: (message: string) => void;
  onPlaybackResult?: (element: HTMLMediaElement, blocked: boolean) => void;
  appearanceControls?: HTMLElement;
  microphoneControls?: HTMLElement;
}

interface PersonalPlaybackPreferences {
  volume: number;
  muted: boolean;
  hidden: boolean;
  quality: RemoteVideoQuality;
}

interface TilePlayback {
  participantId: string;
  participantName: string;
  controls: HTMLDetailsElement;
  disposeControls: () => void;
  blockedNotice: HTMLElement;
  stallNotice: HTMLElement;
  blocked: Map<HTMLMediaElement, HTMLMediaElement['srcObject']>;
  playbackVersion: number;
  /** When a check last saw a new frame presented. */
  video: { progressAt: number } | null;
}

/** A camera tile without a new frame for this long says its video stopped. */
const VIDEO_STALL_MS = 6_000;
const VIDEO_CHECK_MS = 2_000;

/** One pending frame callback per video; set when a frame was presented. */
interface FrameWatch {
  pending: boolean;
  presented: boolean;
}

const MASTER_VOLUME_KEY = 'simplestchat.masterVolume';
let tileMenuSequence = 0;

function readStored(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function writeStored(key: string, value: string): void {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* Preferences still work for this session. */
  }
}

function volumeValue(value: number): number {
  return Number.isFinite(value) ? Math.max(0, Math.min(1, value)) : 1;
}

/** Keep tile actions in the top layer, with viewport listeners owned only while open. */
function configureTileMenu(
  details: HTMLDetailsElement,
  summary: HTMLElement,
  panel: HTMLElement,
): () => void {
  let viewportListeners: AbortController | null = null;
  const position = () => {
    const viewport = window.visualViewport;
    const left = viewport?.offsetLeft ?? 0;
    const top = viewport?.offsetTop ?? 0;
    const width = viewport?.width ?? window.innerWidth;
    const height = viewport?.height ?? window.innerHeight;
    panel.style.maxWidth = `${Math.max(0, width - 16)}px`;
    panel.style.maxHeight = `min(28rem, ${Math.max(0, height - 16)}px)`;
    const anchor = summary.getBoundingClientRect();
    const bounds = panel.getBoundingClientRect();
    panel.style.left = `${Math.max(left + 8, Math.min(anchor.right - bounds.width, left + width - bounds.width - 8))}px`;
    const below = anchor.bottom + 4;
    const preferred =
      below + bounds.height <= top + height - 8 ? below : anchor.top - bounds.height - 4;
    panel.style.top = `${Math.max(top + 8, Math.min(preferred, top + height - bounds.height - 8))}px`;
  };
  const close = () => {
    if (panel.matches(':popover-open')) panel.hidePopover();
    viewportListeners?.abort();
    viewportListeners = null;
    details.open = false;
    summary.setAttribute('aria-expanded', 'false');
  };
  panel.addEventListener('beforetoggle', (event) => {
    // Escape, outside taps and another tile's auto-popover also close the disclosure.
    if (event.newState === 'closed') {
      viewportListeners?.abort();
      viewportListeners = null;
      details.open = false;
      summary.setAttribute('aria-expanded', 'false');
    }
  });
  summary.addEventListener('click', (event) => {
    event.preventDefault();
    if (!details.isConnected) return;
    if (panel.matches(':popover-open')) {
      close();
      return;
    }
    details.open = true;
    summary.setAttribute('aria-expanded', 'true');
    panel.showPopover({ source: summary });
    position();
    viewportListeners = new AbortController();
    const options = { signal: viewportListeners.signal, passive: true };
    window.addEventListener('resize', position, options);
    window.addEventListener('scroll', position, { ...options, capture: true });
    window.visualViewport?.addEventListener('resize', position, options);
    window.visualViewport?.addEventListener('scroll', position, options);
    panel.focus({ preventScroll: true });
  });
  const dismiss = document.createElement('button');
  dismiss.type = 'button';
  dismiss.className = 'personal-media-dismiss';
  dismiss.textContent = 'Close';
  dismiss.addEventListener('click', () => {
    close();
    summary.focus({ preventScroll: true });
  });
  panel.append(dismiss);
  return close;
}

/** Apply explicit device edits without opening inactive capture or crossing room lifecycles. */
export async function applyCaptureSettings(
  room: MediaControlsRoom | null,
  next: CapturePreferences,
  previous: CapturePreferences,
  isCurrent: () => boolean,
): Promise<boolean> {
  if (!isCurrent()) return false;
  const videoKeys: (keyof CapturePreferences)[] = ['cameraDeviceId', 'resolution', 'frameRate'];
  const audioKeys: (keyof CapturePreferences)[] = [
    'microphoneDeviceId',
    'echoCancellation',
    'noiseSuppression',
    'autoGainControl',
  ];
  const changed = (keys: (keyof CapturePreferences)[]) =>
    keys.some((key) => next[key] !== previous[key]);
  const confirmed = { ...next };
  const copy = (keys: (keyof CapturePreferences)[], from: CapturePreferences) => {
    Object.assign(confirmed, Object.fromEntries(keys.map((key) => [key, from[key]])));
  };
  if (room?.videoEnabled && changed(videoKeys)) copy(videoKeys, previous);
  if (room?.audioEnabled && changed(audioKeys)) copy(audioKeys, previous);
  try {
    room?.setCapturePreferences(next);
    saveCapturePreferences(next);
    if (room?.videoEnabled && changed(videoKeys)) {
      await room.switchCamera(next.cameraDeviceId);
      if (!isCurrent()) return false;
    }
    copy(videoKeys, next);
    if (room?.audioEnabled && changed(audioKeys)) {
      await room.switchMic(next.microphoneDeviceId);
      if (!isCurrent()) return false;
    }
    return isCurrent();
  } catch (error) {
    // Keep successful edits, but don't make failed/unattempted active changes the
    // saved baseline: reopening must still let the user select and retry them.
    if (isCurrent()) {
      room?.setCapturePreferences(confirmed);
      saveCapturePreferences(confirmed);
    }
    throw error;
  }
}

/** Adjust this browser's playback only; never mutate a publisher's track. */
export function applyPersonalPlayback(
  elements: Iterable<HTMLMediaElement>,
  preferences: Pick<PersonalPlaybackPreferences, 'volume' | 'muted' | 'hidden'>,
  masterVolume: number,
  onPlaybackResult?: (element: HTMLMediaElement, error?: unknown) => void,
): void {
  for (const element of elements) {
    element.volume = volumeValue(preferences.volume) * volumeValue(masterVolume);
    element.muted = preferences.muted || preferences.hidden;
    if (preferences.hidden) element.pause();
    else if (element.paused && element.srcObject) {
      // Invoke play synchronously so a retry retains the button's user activation.
      element.play().then(
        () => onPlaybackResult?.(element),
        (error) => onPlaybackResult?.(element, error),
      );
    }
  }
}

/** Owns a preview capture independently from room producers. Closing invalidates late permissions. */
export class MediaPreview {
  private generation = 0;
  private starting = false;
  private stream: MediaStream | null = null;
  private audioContext: AudioContext | null = null;
  private source: MediaStreamAudioSourceNode | null = null;
  private frame: number | null = null;

  constructor(
    private onStream: (stream: MediaStream | null) => void,
    private onLevel: (level: number) => void,
  ) {}

  get pending(): boolean {
    return this.starting;
  }

  async start(preferences: CapturePreferences, video = true, audio = true): Promise<boolean> {
    // A permission prompt cannot be cancelled through getUserMedia. Keep it
    // single-flight even when Stop has retired its eventual result.
    if (this.starting) return false;
    this.stop();
    const generation = this.generation;
    if (!video && !audio) throw new Error('Select a camera or microphone to preview.');
    this.starting = true;
    try {
      if (!navigator.mediaDevices?.getUserMedia)
        throw new Error('Media preview requires localhost or a secure connection.');
      const stream = await captureMedia(
        {
          video: video ? captureConstraints(preferences, 'video') : false,
          audio: audio ? captureConstraints(preferences, 'audio') : false,
        },
        () => generation === this.generation,
      );
      if (generation !== this.generation) {
        stream.getTracks().forEach((track) => track.stop());
        return false;
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
          context.resume().catch(() => {});
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
          this.audioContext?.close().catch(() => {});
          this.audioContext = null;
        }
      }
      return true;
    } catch (error) {
      if (generation !== this.generation) return false;
      this.stop();
      throw error;
    } finally {
      this.starting = false;
    }
  }

  stop(): void {
    this.generation++;
    if (this.frame !== null) cancelAnimationFrame(this.frame);
    this.frame = null;
    this.source?.disconnect();
    this.source = null;
    this.audioContext?.close().catch(() => {});
    this.audioContext = null;
    this.stream?.getTracks().forEach((track) => track.stop());
    this.stream = null;
    this.onStream(null);
    this.onLevel(0);
  }
}

export class MediaControls {
  private tiles = new Map<HTMLElement, TilePlayback>();
  private playback = new Map<string, PersonalPlaybackPreferences>();
  private toolbars = new Set<HTMLElement>();
  private preview: MediaPreview | null = null;
  private dialog: HTMLDialogElement | null = null;
  private finishSetup: ((saved: boolean) => void) | null = null;
  private masterVolume = volumeValue(Number(readStored(MASTER_VOLUME_KEY) ?? 1));
  private speakerTest: SpeakerTest | null = null;
  private devices: ReturnType<typeof observeMediaDevices> | null = null;
  private outputWarning = false;
  private deviceRefresh = false;
  private lifecycleVersion = 0;
  private stallTimer: ReturnType<typeof setInterval> | null = null;
  private readonly frameWatches = new WeakMap<HTMLVideoElement, FrameWatch>();
  private readonly output = new AudioOutput(() => [
    ...Array.from(this.tiles.keys()).flatMap((tile) =>
      Array.from(tile.querySelectorAll<HTMLMediaElement>('audio, video')),
    ),
    ...(this.speakerTest ? [this.speakerTest.element] : []),
  ]);

  constructor(private options: MediaControlsOptions) {}

  /** Private previews and tones never resume themselves after backgrounding. */
  setPageActive(active: boolean): void {
    if (active) return;
    this.preview?.stop();
    this.speakerTest?.stop();
  }

  /** Reapply only this viewer's choices after the browser pauses existing media. */
  resumePlayback(): void {
    if (document.visibilityState === 'hidden') return;
    for (const info of this.tiles.values()) info.video = null;
    for (const id of this.playback.keys()) this.applyParticipant(id);
  }

  /** A disconnected Bluetooth/output device must not silently select a new speaker. */
  refreshDevices(): void {
    this.devices?.refresh();
    const selected = this.output.selected;
    if (!selected || this.deviceRefresh || this.tiles.size === 0) return;
    const version = this.lifecycleVersion;
    this.deviceRefresh = true;
    Promise.resolve()
      .then(() => navigator.mediaDevices?.enumerateDevices() ?? [])
      .then((devices) => {
        if (version !== this.lifecycleVersion || selected !== this.output.selected) return;
        const present = devices.some(
          (device) => device.kind === 'audiooutput' && device.deviceId === selected,
        );
        if (!present && !this.outputWarning) {
          this.outputWarning = true;
          this.options.notify(
            'Your selected speaker is not currently listed. Reconnect it or choose a speaker in Your settings.',
          );
        } else if (present) this.outputWarning = false;
      })
      .catch(() => {
        // Enumeration can be unavailable or permission-filtered; it never
        // authorizes a fallback device or changes the user's output choice.
      })
      .finally(() => {
        this.deviceRefresh = false;
      });
  }

  mountToolbar(container: HTMLElement): void {
    const toolbar = document.createElement('div');
    toolbar.className = 'media-controls-toolbar';
    const label = document.createElement('label');
    label.textContent = 'Room volume';
    const slider = document.createElement('input');
    slider.type = 'range';
    slider.min = '0';
    slider.max = '100';
    slider.step = '1';
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
    toolbar.append(label);
    const receiveLabel = document.createElement('label');
    receiveLabel.textContent = 'Incoming video';
    const receive = document.createElement('select');
    receive.setAttribute('aria-label', 'Incoming video');
    receive.title =
      'Balanced: up to 9 videos. Data saver: up to 4 at low quality. Audio only: no incoming video. Your camera and microphone are unchanged.';
    for (const [value, title] of [
      ['balanced', 'Balanced'],
      ['data-saver', 'Data saver'],
      ['audio-only', 'Audio only'],
    ]) {
      receive.add(new Option(title, value));
    }
    receive.value = loadReceiveMode();
    receive.addEventListener('change', () => {
      const mode = normalizeReceiveMode(receive.value);
      saveReceiveMode(mode);
      this.options.getRoom()?.setReceiveMode(mode);
      for (const mounted of this.toolbars) {
        const select = mounted.querySelector('select');
        if (select) select.value = mode;
      }
    });
    receiveLabel.append(receive);
    toolbar.append(receiveLabel);
    container.append(toolbar);
    this.toolbars.add(toolbar);
  }

  /** Opens without capture. Saving may switch active devices, but never enables an inactive one. */
  openSetup(mode: 'microphone' | 'settings' = 'settings'): Promise<boolean> {
    this.closeSetup(false);
    let preferences = loadCapturePreferences();
    const dialog = document.createElement('dialog');
    dialog.className = 'settings-dialog media-setup-dialog';
    dialog.setAttribute('aria-labelledby', 'media-setup-title');
    // Static copy chosen by the opening control; no user content enters this markup.
    const intro =
      mode === 'microphone'
        ? 'Pick a microphone and try it privately. Saving never turns it on by itself.'
        : 'Just for you, on this browser.';
    dialog.innerHTML = `
      <form method="dialog" class="settings-dialog-form">
        <div class="settings-dialog-header"><div><h2 id="media-setup-title">Your settings</h2>
          <p class="settings-description">${intro}</p></div>
          <button type="button" data-dialog-close aria-label="Close your settings">Close</button></div>
        <div class="settings-dialog-nav" role="tablist" aria-label="Your settings sections">
          <button type="button" role="tab" id="media-devices-tab" data-settings-tab aria-controls="media-devices-panel">Audio &amp; video</button>
          <button type="button" role="tab" id="media-appearance-tab" data-settings-tab aria-controls="media-appearance-panel">Appearance</button>
        </div>
        <div class="settings-dialog-body">
          <section id="media-devices-panel" role="tabpanel" aria-labelledby="media-devices-tab">
            <h3 class="settings-section-heading">Camera &amp; microphone</h3>
            <p class="settings-description">Saving updates active devices. Devices that are off stay off.</p>
            <div class="media-setup-fields">
              <div class="media-device-field">
                <label>Camera <select name="cameraDeviceId" aria-describedby="media-device-permission-hint"><option value="">Default camera</option></select></label>
                <button type="button" data-action="test-camera">Test camera</button>
              </div>
              <div class="media-device-field">
                <label>Microphone <select name="microphoneDeviceId" aria-describedby="media-device-permission-hint"><option value="">Default microphone</option></select></label>
                <button type="button" data-action="test-microphone">Test microphone</button>
              </div>
            </div>
            <p id="media-device-permission-hint" class="media-setup-hint">Device names may stay hidden until you allow access. Test a device to allow it and refresh its list now, without saving or broadcasting.</p>
            <p class="media-preview-status" role="status">Tests and preview are private. Only you can see the camera or microphone level.</p>
            <section class="media-speaker-panel" aria-label="Speaker settings">
              <label>Speaker <select data-output-device><option value="">System default</option></select></label>
              <div class="media-preview-actions"><button type="button" data-output-apply>Use speaker</button>
                <button type="button" data-output-choose>Choose another speaker</button>
                <button type="button" data-output-test>Test speaker</button></div>
              <p data-output-status role="status" class="settings-description">Speaker changes apply immediately for this tab. The test plays a short tone.</p>
            </section>
            <div class="media-microphone-controls"></div>
            <section class="media-preview-panel" aria-label="Private preview">
              <div class="media-preview-heading"><h3 class="settings-section-heading">Try your devices</h3><span class="media-private-badge">Only you</span></div>
              <video class="media-preview-video" autoplay muted playsinline hidden></video>
              <div class="media-preview-options">
                <label><input type="checkbox" name="previewCamera"> Camera preview</label>
                <label><input type="checkbox" name="previewMicrophone" checked> Microphone preview</label>
              </div>
              <label class="media-meter-label">Microphone level <meter min="0" max="1" value="0" aria-label="Microphone input level"></meter></label>
              <div class="media-preview-actions"><button type="button" data-action="preview">Start preview</button>
                <button type="button" data-action="stop" disabled>Stop preview</button></div>
            </section>
            <details class="media-advanced"><summary>Advanced capture settings</summary>
              <p class="settings-description">The defaults work well for most connections.</p>
              <div class="media-setup-fields">
                <label>Resolution <select name="resolution"><option value="360p">360p — less bandwidth</option><option value="720p">720p — balanced</option><option value="1080p">1080p — sharper video</option></select></label>
                <label>Frame rate <select name="frameRate"><option value="15">15 fps</option><option value="30">30 fps</option><option value="60">60 fps</option></select></label>
              </div>
              <fieldset class="media-audio-options"><legend>Microphone processing</legend>
                <label><input type="checkbox" name="echoCancellation"> Echo cancellation</label>
                <label><input type="checkbox" name="noiseSuppression"> Noise suppression</label>
                <label><input type="checkbox" name="autoGainControl"> Automatic level</label>
              </fieldset>
            </details>
          </section>
          <section id="media-appearance-panel" role="tabpanel" aria-labelledby="media-appearance-tab" hidden></section>
        </div>
        <div class="settings-dialog-footer">
          <p class="settings-description">Camera and microphone changes apply on Save. Layout and talk mode save automatically.</p>
          <button type="submit" value="save" class="btn-primary">Save settings</button>
        </div>
      </form>`;
    if (this.options.microphoneControls)
      dialog.querySelector('.media-microphone-controls')!.append(this.options.microphoneControls);
    if (this.options.appearanceControls)
      dialog.querySelector('#media-appearance-panel')!.append(this.options.appearanceControls);
    else dialog.querySelector('#media-appearance-tab')!.remove();
    const field = <T extends HTMLInputElement | HTMLSelectElement>(name: string) =>
      dialog.querySelector<T>(`[name="${name}"]`)!;
    field<HTMLInputElement>('previewCamera').checked = mode !== 'microphone';
    for (const name of ['cameraDeviceId', 'microphoneDeviceId'] as const) {
      if (preferences[name])
        field<HTMLSelectElement>(name).add(new Option('Saved device', preferences[name]));
      field<HTMLSelectElement>(name).value = preferences[name];
    }
    for (const name of ['resolution', 'frameRate'] as const)
      field<HTMLSelectElement>(name).value = String(preferences[name]);
    for (const name of ['echoCancellation', 'noiseSuppression', 'autoGainControl'] as const)
      field<HTMLInputElement>(name).checked = preferences[name];
    const status = dialog.querySelector<HTMLElement>('.media-preview-status')!;
    const video = dialog.querySelector('video')!;
    video.muted = true;
    const meter = dialog.querySelector('meter')!;
    const readPreferences = (): CapturePreferences =>
      normalizeCapturePreferences({
        cameraDeviceId: field<HTMLSelectElement>('cameraDeviceId').value,
        microphoneDeviceId: field<HTMLSelectElement>('microphoneDeviceId').value,
        resolution: field<HTMLSelectElement>('resolution').value,
        frameRate: Number(field<HTMLSelectElement>('frameRate').value),
        echoCancellation: field<HTMLInputElement>('echoCancellation').checked,
        noiseSuppression: field<HTMLInputElement>('noiseSuppression').checked,
        autoGainControl: field<HTMLInputElement>('autoGainControl').checked,
      });
    const output = dialog.querySelector<HTMLSelectElement>('[data-output-device]')!;
    const outputStatus = dialog.querySelector<HTMLElement>('[data-output-status]')!;
    const outputApply = dialog.querySelector<HTMLButtonElement>('[data-output-apply]')!;
    const outputChoose = dialog.querySelector<HTMLButtonElement>('[data-output-choose]')!;
    const outputTest = dialog.querySelector<HTMLButtonElement>('[data-output-test]')!;
    const picker = navigator.mediaDevices as MediaDevices & {
      selectAudioOutput?: () => Promise<MediaDeviceInfo>;
    };
    output.disabled = outputApply.disabled = !this.output.supported;
    outputChoose.hidden = !this.output.supported || typeof picker?.selectAudioOutput !== 'function';
    if (!this.output.supported)
      outputStatus.textContent =
        'This browser uses your system speaker. Change output in your system sound settings.';
    if (this.output.selected) output.add(new Option('Selected speaker', this.output.selected));
    output.value = this.output.selected;
    let outputAction = 0;
    const applyOutput = async (deviceId: string, action: number) => {
      if (this.dialog !== dialog || action !== outputAction) return;
      await this.output.change(deviceId);
      if (this.dialog !== dialog || action !== outputAction) return;
      this.outputWarning = false;
      for (const id of this.playback.keys()) this.applyParticipant(id);
      outputStatus.textContent = 'Speaker selected for this tab. Use Test speaker to check it.';
    };
    const outputActionStart = (choose: boolean) => {
      const action = ++outputAction;
      this.speakerTest?.stop();
      outputApply.disabled = outputChoose.disabled = outputTest.disabled = true;
      outputStatus.textContent = choose ? 'Choose a speaker in your browser…' : 'Changing speaker…';
      // The picker is invoked before any await, preserving this button's gesture.
      let selected: Promise<MediaDeviceInfo | null>;
      try {
        selected =
          choose && picker.selectAudioOutput ? picker.selectAudioOutput() : Promise.resolve(null);
      } catch (error) {
        selected = Promise.reject(
          error instanceof Error ? error : new Error('Speaker selection failed'),
        );
      }
      selected
        .then(async (device) => {
          if (this.dialog !== dialog || action !== outputAction) return;
          if (device) {
            output.add(new Option(device.label || 'Selected speaker', device.deviceId));
            output.value = device.deviceId;
          }
          await applyOutput(output.value, action);
          this.devices?.refresh();
        })
        .catch((error) => {
          if (this.dialog === dialog && action === outputAction)
            outputStatus.textContent = outputErrorMessage(error);
        })
        .finally(() => {
          if (this.dialog === dialog && action === outputAction)
            outputApply.disabled = outputChoose.disabled = outputTest.disabled = false;
        });
    };
    outputApply.addEventListener('click', () => outputActionStart(false));
    outputChoose.addEventListener('click', () => outputActionStart(true));
    outputTest.addEventListener('click', () => {
      const test = this.speakerTest;
      if (!test) return;
      test
        .play()
        .then(() => {
          if (this.dialog === dialog)
            outputStatus.textContent =
              'Test tone played. If you did not hear it, check your speaker selection and system volume.';
        })
        .catch((error) => {
          if (this.dialog === dialog) outputStatus.textContent = outputErrorMessage(error);
        });
    });
    const fillDevices = (devices: MediaDeviceInfo[]) => {
      if (this.dialog !== dialog) return;
      for (const [name, kind, title] of [
        ['cameraDeviceId', 'videoinput', 'camera'],
        ['microphoneDeviceId', 'audioinput', 'microphone'],
      ] as const) {
        const select = field<HTMLSelectElement>(name);
        const selected = select.value;
        select.replaceChildren(new Option(`Default ${title}`, ''));
        const matching = devices.filter((device) => device.kind === kind && device.deviceId);
        for (const [index, device] of matching.entries())
          select.add(new Option(device.label || `${title} ${index + 1}`, device.deviceId));
        if (selected && !matching.some((device) => device.deviceId === selected)) {
          // Device names/IDs may be concealed until the user grants preview permission.
          select.add(new Option(`Saved ${title} (not currently listed)`, selected));
        }
        select.value = selected;
      }
      const selected = output.value;
      output.replaceChildren(new Option('System default', ''));
      const speakers = devices.filter((device) => device.kind === 'audiooutput' && device.deviceId);
      for (const [index, device] of speakers.entries())
        output.add(new Option(device.label || `Speaker ${index + 1}`, device.deviceId));
      if (selected && !speakers.some((device) => device.deviceId === selected)) {
        output.add(new Option('Selected speaker (not currently listed)', selected));
        if (selected === this.output.selected)
          outputStatus.textContent =
            'Selected speaker is not currently listed. Choose another output or use the system default.';
      }
      output.value = selected;
    };
    const preview = new MediaPreview(
      (stream) => {
        video.srcObject = stream;
        video.hidden = !stream?.getVideoTracks().length;
        if (stream) video.play().catch(() => {});
      },
      (level) => {
        meter.value = level;
      },
    );
    this.preview = preview;
    this.dialog = dialog;
    try {
      this.speakerTest = new SpeakerTest();
      outputTest.disabled = true;
      this.output
        .attach(this.speakerTest.element)
        .then(() => {
          if (this.dialog === dialog) outputTest.disabled = false;
        })
        .catch((error) => {
          if (this.dialog === dialog) outputStatus.textContent = outputErrorMessage(error);
        });
    } catch {
      outputTest.disabled = true;
      outputStatus.textContent = 'Speaker testing is unavailable in this browser.';
    }
    this.devices = observeMediaDevices(fillDevices, () => {
      if (this.dialog === dialog)
        status.textContent = 'Device list unavailable. You can still try the default devices.';
    });
    const previewButton = dialog.querySelector<HTMLButtonElement>('[data-action="preview"]')!;
    const cameraTest = dialog.querySelector<HTMLButtonElement>('[data-action="test-camera"]')!;
    const microphoneTest = dialog.querySelector<HTMLButtonElement>(
      '[data-action="test-microphone"]',
    )!;
    const startButtons = [previewButton, cameraTest, microphoneTest];
    const stopButton = dialog.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
    const saveButton = dialog.querySelector<HTMLButtonElement>('[type="submit"]')!;
    let previewAction = 0;
    let saving = false;
    const updateCaptureButtons = () => {
      const busy = saving || preview.pending;
      for (const button of startButtons) button.disabled = busy;
      saveButton.disabled = busy;
    };
    const stopPreview = () => {
      previewAction++;
      preview.stop();
      updateCaptureButtons();
      stopButton.disabled = true;
      status.textContent = preview.pending
        ? 'Preview cancelled. Dismiss the browser permission prompt before testing again.'
        : 'Preview stopped.';
    };
    stopButton.addEventListener('click', stopPreview);
    dialog.addEventListener('settings-tab-change', (event) => {
      if ((event as CustomEvent<string>).detail === 'media-appearance-panel') {
        stopPreview();
        this.speakerTest?.stop();
      }
    });
    const startPreview = (camera: boolean, microphone: boolean) => {
      if (this.dialog !== dialog || saving || preview.pending) return;
      const action = ++previewAction;
      field<HTMLInputElement>('previewCamera').checked = camera;
      field<HTMLInputElement>('previewMicrophone').checked = microphone;
      const device =
        camera && microphone
          ? 'Camera and microphone'
          : camera
            ? 'Camera'
            : microphone
              ? 'Microphone'
              : 'Preview';
      const start = async () => {
        stopButton.disabled = false;
        status.textContent = `${device}: waiting for browser permission…`;
        try {
          // Start within this click's turn, requesting only the chosen kinds.
          const capture = preview.start(readPreferences(), camera, microphone);
          updateCaptureButtons();
          const started = await capture;
          if (this.dialog !== dialog || action !== previewAction) return;
          if (!started) return;
          status.textContent = camera
            ? microphone
              ? 'Preview is private. Check the picture and microphone level below; no microphone sound plays through the speakers.'
              : 'Camera preview is private. Check the picture below and choose a camera above.'
            : 'Microphone test is private. Check the level below; your microphone is not played through the speakers.';
          this.devices?.refresh();
        } catch (error) {
          if (this.dialog !== dialog || action !== previewAction) return;
          stopButton.disabled = true;
          status.textContent = mediaErrorMessage(error, device);
        } finally {
          if (this.dialog === dialog) updateCaptureButtons();
        }
      };
      start().catch((error) => {
        if (this.dialog === dialog && action === previewAction)
          status.textContent = mediaErrorMessage(error, device);
      });
    };
    cameraTest.addEventListener('click', () => startPreview(true, false));
    microphoneTest.addEventListener('click', () => startPreview(false, true));
    previewButton.addEventListener('click', () => {
      startPreview(
        field<HTMLInputElement>('previewCamera').checked,
        field<HTMLInputElement>('previewMicrophone').checked,
      );
    });
    dialog.querySelector('form')!.addEventListener('change', (event) => {
      // Layout and microphone mode apply immediately, independently of capture drafts.
      if (!(event.target instanceof Element) || !event.target.matches('[name]')) return;
      stopPreview();
      if (!preview.pending)
        status.textContent = 'Settings changed. Test a device or start preview to try them.';
    });
    dialog.querySelector('form')!.addEventListener('submit', (event) => {
      event.preventDefault();
      if (saving || preview.pending) return;
      const save = async () => {
        const next = readPreferences();
        const activeRoom = this.options.getRoom();
        const isCurrent = () => this.dialog === dialog && this.options.getRoom() === activeRoom;
        previewAction++;
        preview.stop();
        stopButton.disabled = true;
        saving = true;
        updateCaptureButtons();
        saveButton.textContent = 'Saving…';
        const captureFields = dialog.querySelectorAll<HTMLInputElement | HTMLSelectElement>(
          'input[name], select[name]',
        );
        captureFields.forEach((control) => {
          control.disabled = true;
        });
        try {
          if (!(await applyCaptureSettings(activeRoom, next, preferences, isCurrent))) return;
          this.closeSetup(true);
        } catch (error) {
          if (!isCurrent()) return;
          preferences = loadCapturePreferences();
          status.textContent = mediaErrorMessage(error);
          dialog.querySelector<HTMLButtonElement>('#media-devices-tab')!.click();
        } finally {
          if (isCurrent()) {
            saving = false;
            updateCaptureButtons();
            saveButton.textContent = 'Save settings';
            captureFields.forEach((control) => {
              control.disabled = false;
            });
          }
        }
      };
      save().catch((error) => {
        if (this.dialog === dialog) status.textContent = mediaErrorMessage(error);
      });
    });
    configureSettingsDialog(dialog, () => this.closeSetup(false));
    dialog.addEventListener('close', () => {
      if (this.dialog === dialog) this.closeSetup(false);
    });
    document.body.append(dialog);
    const result = new Promise<boolean>((resolve) => {
      this.finishSetup = resolve;
    });
    try {
      dialog.showModal();
    } catch (error) {
      this.closeSetup(false);
      this.options.notify(mediaErrorMessage(error));
    }
    return result;
  }

  /** Call after each added/replaced remote audio/video element, including screen shares. */
  attachTile(tile: HTMLElement, participantId: string, participantName: string): void {
    // Removed tiles can be replaced when a producer pauses/resumes.
    for (const [element, info] of this.tiles) {
      if (!element.isConnected) {
        info.disposeControls();
        this.tiles.delete(element);
      }
    }
    if (!this.playback.has(participantId))
      this.playback.set(participantId, { volume: 1, muted: false, hidden: false, quality: 'auto' });
    if (!this.tiles.has(tile)) {
      const details = document.createElement('details');
      details.className = 'personal-media-controls';
      const summary = document.createElement('summary');
      summary.setAttribute('aria-label', `Controls for ${participantName}`);
      summary.setAttribute('aria-haspopup', 'dialog');
      summary.setAttribute('aria-expanded', 'false');
      summary.title = `Controls for ${participantName}`;
      const icon = document.createElement('span');
      icon.textContent = '⋯';
      icon.setAttribute('aria-hidden', 'true');
      summary.append(icon);
      const panel = document.createElement('div');
      panel.className = 'personal-media-panel';
      panel.id = `personal-media-panel-${++tileMenuSequence}`;
      panel.setAttribute('popover', 'auto');
      panel.setAttribute('role', 'dialog');
      panel.setAttribute('aria-label', `Viewing controls for ${participantName}`);
      panel.tabIndex = -1;
      summary.setAttribute('aria-controls', panel.id);
      const heading = document.createElement('strong');
      heading.className = 'personal-media-title';
      heading.textContent = participantName;
      panel.append(heading);
      const pin = tile.querySelector<HTMLButtonElement>('.tile-pin');
      if (pin) panel.append(pin);
      const volumeLabel = document.createElement('label');
      volumeLabel.textContent = 'Volume';
      const volume = document.createElement('input');
      volume.type = 'range';
      volume.min = '0';
      volume.max = '100';
      volume.step = '1';
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
      const hide = this.button('Hide for me', () =>
        this.setHidden(participantId, !this.playback.get(participantId)!.hidden),
      );
      hide.dataset['control'] = 'hide';
      const qualityLabel = document.createElement('label');
      qualityLabel.textContent = 'Video quality';
      const quality = document.createElement('select');
      quality.dataset['control'] = 'quality';
      quality.setAttribute('aria-label', `Video quality for ${participantName}`);
      for (const [value, title] of [
        ['auto', 'Auto'],
        ['low', 'Low'],
        ['medium', 'Medium'],
        ['high', 'High'],
      ])
        quality.add(new Option(title, value));
      quality.title =
        'Limits camera quality when the broadcast offers multiple layers. Screen shares use their original quality.';
      quality.addEventListener('change', () => {
        this.playback.get(participantId)!.quality = quality.value as RemoteVideoQuality;
        this.options
          .getRoom()
          ?.setRemoteVideoQuality(participantId, quality.value as RemoteVideoQuality);
        this.applyParticipant(participantId);
      });
      qualityLabel.append(quality);
      const fullscreen = this.button('Fullscreen', () => {
        disposeControls();
        tile
          .requestFullscreen()
          .catch(() => this.options.notify('Fullscreen is unavailable for this video.'));
      });
      fullscreen.dataset['control'] = 'fullscreen';
      fullscreen.hidden = typeof tile.requestFullscreen !== 'function';
      const pip = this.button('Picture in picture', () => {
        const video = tile.querySelector('video');
        if (!video || typeof video.requestPictureInPicture !== 'function') return;
        disposeControls();
        const action =
          document.pictureInPictureElement === video
            ? document.exitPictureInPicture()
            : video.requestPictureInPicture();
        action.catch(() =>
          this.options.notify('Picture in picture is unavailable for this video.'),
        );
      });
      pip.dataset['control'] = 'pip';
      panel.append(volumeLabel, mute, hide, qualityLabel, fullscreen, pip);
      details.append(summary, panel);
      const disposeControls = configureTileMenu(details, summary, panel);
      pin?.addEventListener('click', disposeControls);
      // Keep viewing controls from opening the tile's moderation context menu.
      details.addEventListener('contextmenu', (event) => event.stopPropagation());
      const hiddenNotice = document.createElement('div');
      hiddenNotice.className = 'personal-media-hidden-notice';
      hiddenNotice.hidden = true;
      const text = document.createElement('span');
      text.textContent = `${participantName}'s broadcast is hidden for you.`;
      hiddenNotice.append(
        text,
        this.button('Restore broadcast', () => this.setHidden(participantId, false)),
      );
      const blockedNotice = document.createElement('div');
      blockedNotice.className = 'personal-playback-blocked';
      blockedNotice.hidden = true;
      blockedNotice.setAttribute('role', 'status');
      const explanation = document.createElement('span');
      explanation.textContent = 'Your browser blocked playback.';
      const retry = this.button('Enable playback', () => {
        if (this.tiles.has(tile) && tile.isConnected) this.applyParticipant(participantId, tile);
      });
      retry.dataset['control'] = 'retry-playback';
      retry.setAttribute('aria-label', `Enable playback for ${participantName}`);
      blockedNotice.append(explanation, retry);
      const stallNotice = document.createElement('div');
      stallNotice.className = 'video-stalled-notice';
      stallNotice.hidden = true;
      stallNotice.setAttribute('role', 'status');
      tile.append(details, hiddenNotice, blockedNotice, stallNotice);
      this.tiles.set(tile, {
        participantId,
        participantName,
        controls: details,
        disposeControls,
        blockedNotice,
        stallNotice,
        blocked: new Map(),
        playbackVersion: 0,
        video: null,
      });
      this.stallTimer ??= setInterval(() => this.checkVideoProgress(), VIDEO_CHECK_MS);
    }
    this.applyParticipant(participantId);
    const state = this.playback.get(participantId)!;
    if (state.hidden) this.options.getRoom()?.setRemoteMediaHidden(participantId, true);
    if (state.quality !== 'auto')
      this.options.getRoom()?.setRemoteVideoQuality(participantId, state.quality);
  }

  /** Retire one camera/screen tile without clearing its owner's playback preferences. */
  detachTile(tile: HTMLElement): void {
    const info = this.tiles.get(tile);
    if (!info) return;
    info.disposeControls();
    info.controls.remove();
    info.blockedNotice.remove();
    info.stallNotice.remove();
    tile.querySelector('.personal-media-hidden-notice')?.remove();
    this.tiles.delete(tile);
  }

  detachParticipant(participantId: string): void {
    for (const [tile, info] of this.tiles)
      if (info.participantId === participantId) this.detachTile(tile);
    this.playback.delete(participantId);
  }

  /**
   * A remote camera that stops delivering frames leaves its last frame on
   * screen, which looks live when the scene is still; say so once it has been
   * still for VIDEO_STALL_MS. Screen shares send frames only when the screen
   * changes, background tabs do not render, and a paused camera's video is
   * removed, so none of those count.
   */
  checkVideoProgress(now = Date.now()): void {
    const visible = document.visibilityState !== 'hidden';
    for (const [tile, info] of this.tiles) {
      const video = tile.querySelector('video');
      const presented =
        visible &&
        video &&
        !video.paused &&
        !tile.classList.contains('screen-share') &&
        !this.playback.get(info.participantId)?.hidden
          ? this.framePresented(video)
          : null;
      if (presented === null || !info.video || presented) {
        info.video = presented === null ? null : { progressAt: now };
        info.stallNotice.hidden = true;
      } else if (now - info.video.progressAt >= VIDEO_STALL_MS && info.stallNotice.hidden) {
        const stoppedAt = new Date(info.video.progressAt).toLocaleTimeString([], {
          hour: '2-digit',
          minute: '2-digit',
        });
        info.stallNotice.textContent = `${info.participantName}'s video stopped at ${stoppedAt}.`;
        info.stallNotice.hidden = false;
      }
    }
  }

  /**
   * Whether the video presented a frame since the last call, or null when the
   * browser cannot tell. Firefox counts no MediaStream frames in
   * getVideoPlaybackQuality(), so this waits on one frame callback at a time:
   * a stalled video never calls it, a live one within a frame.
   */
  private framePresented(video: HTMLVideoElement): boolean | null {
    if (typeof video.requestVideoFrameCallback !== 'function') return null;
    let watch = this.frameWatches.get(video);
    if (!watch) {
      watch = { pending: false, presented: true };
      this.frameWatches.set(video, watch);
    }
    const presented = watch.presented;
    watch.presented = false;
    if (!watch.pending) {
      const current = watch;
      current.pending = true;
      video.requestVideoFrameCallback(() => {
        current.pending = false;
        current.presented = true;
      });
    }
    return presented;
  }

  reset(): void {
    this.lifecycleVersion++;
    this.outputWarning = false;
    this.closeSetup(false);
    for (const id of this.playback.keys()) this.detachParticipant(id);
  }

  destroy(): void {
    if (this.stallTimer !== null) clearInterval(this.stallTimer);
    this.stallTimer = null;
    this.reset();
    for (const toolbar of this.toolbars) toolbar.remove();
    this.toolbars.clear();
  }

  private closeSetup(saved: boolean): void {
    this.devices?.dispose();
    this.devices = null;
    this.speakerTest?.dispose();
    this.speakerTest = null;
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

  private updatePlaybackNotice(
    tile: HTMLElement,
    info: TilePlayback,
    state: PersonalPlaybackPreferences,
  ): void {
    const elements = Array.from(tile.querySelectorAll<HTMLMediaElement>('video, audio'));
    for (const [element, source] of info.blocked) {
      if (!elements.includes(element) || element.srcObject !== source || !element.paused)
        info.blocked.delete(element);
    }
    info.blockedNotice.hidden =
      state.hidden ||
      state.muted ||
      state.volume * this.masterVolume === 0 ||
      info.blocked.size === 0;
  }

  private applyParticipant(participantId: string, onlyTile?: HTMLElement): void {
    const state = this.playback.get(participantId);
    if (!state) return;
    for (const [tile, info] of this.tiles) {
      if (info.participantId !== participantId || (onlyTile && tile !== onlyTile)) continue;
      const version = ++info.playbackVersion;
      const elements = Array.from(tile.querySelectorAll<HTMLMediaElement>('video, audio'));
      const sources = new Map(elements.map((element) => [element, element.srcObject]));
      const ready = elements.filter((element) => {
        if (typeof element.setSinkId !== 'function' || element.sinkId === this.output.selected)
          return true;
        // A newly attached stream must not briefly play through a different speaker.
        element.muted = true;
        element.pause();
        this.output
          .attach(element)
          .then(() => {
            if (
              this.tiles.get(tile) === info &&
              tile.isConnected &&
              element.srcObject === sources.get(element)
            )
              this.applyParticipant(participantId, tile);
          })
          .catch(() => {
            if (this.tiles.get(tile) !== info || !tile.isConnected || this.outputWarning) return;
            this.outputWarning = true;
            this.options.notify(
              'Some room audio could not use your speaker. Open Your settings to select an output.',
            );
          });
        return false;
      });
      applyPersonalPlayback(ready, state, this.masterVolume, (element, error) => {
        // A settled promise must not revive controls for a detached tile or an old stream.
        if (
          this.tiles.get(tile) !== info ||
          !tile.isConnected ||
          version !== info.playbackVersion ||
          element.srcObject !== sources.get(element)
        )
          return;
        if (
          error &&
          typeof error === 'object' &&
          'name' in error &&
          error.name === 'NotAllowedError'
        ) {
          info.blocked.set(element, element.srcObject);
        } else info.blocked.delete(element);
        try {
          this.options.onPlaybackResult?.(element, info.blocked.has(element));
        } catch {
          /* Diagnostics cannot interrupt playback controls. */
        }
        this.updatePlaybackNotice(tile, info, state);
      });
      this.updatePlaybackNotice(tile, info, state);
      tile.classList.toggle('personal-media-hidden', state.hidden);
      const notice = tile.querySelector<HTMLElement>('.personal-media-hidden-notice');
      if (notice) notice.hidden = !state.hidden;
      const volume = info.controls.querySelector<HTMLInputElement>('[data-control="volume"]')!;
      volume.value = String(Math.round(state.volume * 100));
      volume.title = `Volume: ${volume.value}%`;
      const mute = info.controls.querySelector<HTMLButtonElement>('[data-control="mute"]')!;
      mute.textContent = state.muted ? 'Unmute for me' : 'Mute for me';
      mute.setAttribute('aria-pressed', String(state.muted));
      info.controls.querySelector('[data-control="hide"]')!.textContent = state.hidden
        ? 'Restore broadcast'
        : 'Hide for me';
      const quality = info.controls.querySelector<HTMLSelectElement>('[data-control="quality"]')!;
      quality.value = state.quality;
      quality.disabled =
        !tile.querySelector('video') || tile.classList.contains('screen-share') || state.hidden;
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

export function mediaErrorMessage(error: unknown, device = 'Camera or microphone'): string {
  if (error instanceof Error) {
    if (error.name === 'NotAllowedError')
      return `${device} access was denied. Allow access in your browser and try again.`;
    if (error.name === 'NotFoundError')
      return `${device} could not be found. Select another device or test one device at a time.`;
    if (error.name === 'OverconstrainedError')
      return 'The selected device is unavailable. Choose a default device and try again.';
    if (error.name === 'NotReadableError')
      return `${device} could not be opened. Try another device in Your settings, or close other apps using it and try again.`;
    if (error.name === 'InvalidStateError')
      return 'Return to this tab and try again. Your browser paused access while the page was inactive.';
    return error.message;
  }
  return 'Could not open your media devices. Please try again.';
}
