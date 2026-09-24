import {
  captureConstraints,
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

interface MediaControlsRoom {
  setCapturePreferences(preferences: CapturePreferences): void;
  readonly audioEnabled: boolean;
  readonly videoEnabled: boolean;
  switchCamera(deviceId: string): Promise<void>;
  switchMic(deviceId: string): Promise<void>;
  setRemoteMediaHidden(participantId: string, hidden: boolean): void;
  setRemoteVideoQuality(participantId: string, quality: RemoteVideoQuality): void;
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
  controls: HTMLDetailsElement;
  blockedNotice: HTMLElement;
  blocked: Map<HTMLMediaElement, HTMLMediaElement['srcObject']>;
  playbackVersion: number;
}

const MASTER_VOLUME_KEY = 'simplestchat.masterVolume';
const SETUP_KEY = 'simplestchat.mediaSetupConfigured';

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
      if (!navigator.mediaDevices?.getUserMedia)
        throw new Error('Media preview requires localhost or a secure connection.');
      const stream = await navigator.mediaDevices.getUserMedia({
        video: video ? captureConstraints(preferences, 'video') : false,
        audio: audio ? captureConstraints(preferences, 'audio') : false,
      });
      if (generation !== this.generation) {
        stream.getTracks().forEach((track) => track.stop());
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
  private configured = readStored(SETUP_KEY) === 'true';
  private masterVolume = volumeValue(Number(readStored(MASTER_VOLUME_KEY) ?? 1));
  private speakerTest: SpeakerTest | null = null;
  private devices: ReturnType<typeof observeMediaDevices> | null = null;
  private outputWarning = false;
  private readonly output = new AudioOutput(() => [
    ...Array.from(this.tiles.keys()).flatMap((tile) =>
      Array.from(tile.querySelectorAll<HTMLMediaElement>('audio, video')),
    ),
    ...(this.speakerTest ? [this.speakerTest.element] : []),
  ]);

  constructor(private options: MediaControlsOptions) {}

  get hasConfiguredSetup(): boolean {
    return this.configured;
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
    container.append(toolbar);
    this.toolbars.add(toolbar);
  }

  /** Opens without capture. Saving may switch active devices, but never enables an inactive one. */
  openSetup(mode: 'camera' | 'microphone' | 'settings' = 'settings'): Promise<boolean> {
    this.closeSetup(false);
    let preferences = loadCapturePreferences();
    const dialog = document.createElement('dialog');
    dialog.className = 'settings-dialog media-setup-dialog';
    dialog.setAttribute('aria-labelledby', 'media-setup-title');
    dialog.innerHTML = `
      <form method="dialog" class="settings-dialog-form">
        <div class="settings-dialog-header"><div><h2 id="media-setup-title">Your settings</h2>
          <p class="settings-description">Just for you, on this browser.</p></div>
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
              <label>Camera <select name="cameraDeviceId"><option value="">Default camera</option></select></label>
              <label>Microphone <select name="microphoneDeviceId"><option value="">Default microphone</option></select></label>
            </div>
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
              <p class="media-preview-status" role="status">Preview is private and does not broadcast to the room.</p>
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
          <p class="settings-description">Device changes apply on Save. Layout and talk mode save automatically.</p>
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
    const stopButton = dialog.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
    const saveButton = dialog.querySelector<HTMLButtonElement>('[type="submit"]')!;
    let previewAction = 0;
    const stopPreview = () => {
      previewAction++;
      preview.stop();
      previewButton.disabled = saveButton.disabled;
      stopButton.disabled = true;
      status.textContent = 'Preview stopped.';
    };
    stopButton.addEventListener('click', stopPreview);
    dialog.addEventListener('settings-tab-change', (event) => {
      if ((event as CustomEvent<string>).detail === 'media-appearance-panel') {
        stopPreview();
        this.speakerTest?.stop();
      }
    });
    previewButton.addEventListener('click', () => {
      const action = ++previewAction;
      const startPreview = async () => {
        previewButton.disabled = true;
        stopButton.disabled = false;
        status.textContent = 'Waiting for device permission…';
        try {
          await preview.start(
            readPreferences(),
            field<HTMLInputElement>('previewCamera').checked,
            field<HTMLInputElement>('previewMicrophone').checked,
          );
          if (this.dialog !== dialog || action !== previewAction) return;
          status.textContent =
            'Preview is local to you. Your microphone is not played through the speakers.';
          this.devices?.refresh();
        } catch (error) {
          if (this.dialog !== dialog || action !== previewAction) return;
          stopButton.disabled = true;
          status.textContent = mediaErrorMessage(error);
        } finally {
          if (this.dialog === dialog && action === previewAction) previewButton.disabled = false;
        }
      };
      startPreview().catch((error) => {
        if (this.dialog === dialog && action === previewAction)
          status.textContent = mediaErrorMessage(error);
      });
    });
    dialog.querySelector('form')!.addEventListener('change', (event) => {
      // Layout and microphone mode apply immediately, independently of capture drafts.
      if (!(event.target instanceof Element) || !event.target.matches('[name]')) return;
      previewAction++;
      preview.stop();
      status.textContent = 'Settings changed. Start preview to try them.';
      previewButton.disabled = false;
      stopButton.disabled = true;
    });
    dialog.querySelector('form')!.addEventListener('submit', (event) => {
      event.preventDefault();
      if (saveButton.disabled) return;
      const save = async () => {
        const next = readPreferences();
        const activeRoom = this.options.getRoom();
        const isCurrent = () => this.dialog === dialog && this.options.getRoom() === activeRoom;
        previewAction++;
        preview.stop();
        stopButton.disabled = true;
        previewButton.disabled = true;
        saveButton.disabled = true;
        saveButton.textContent = 'Saving…';
        const captureFields = dialog.querySelectorAll<HTMLInputElement | HTMLSelectElement>(
          'input[name], select[name]',
        );
        captureFields.forEach((control) => {
          control.disabled = true;
        });
        try {
          if (!(await applyCaptureSettings(activeRoom, next, preferences, isCurrent))) return;
          this.configured = true;
          writeStored(SETUP_KEY, 'true');
          this.closeSetup(true);
        } catch (error) {
          if (!isCurrent()) return;
          preferences = loadCapturePreferences();
          status.textContent = mediaErrorMessage(error);
          dialog.querySelector<HTMLButtonElement>('#media-devices-tab')!.click();
        } finally {
          if (isCurrent()) {
            saveButton.disabled = false;
            saveButton.textContent = 'Save settings';
            previewButton.disabled = false;
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
    for (const [element] of this.tiles) if (!element.isConnected) this.tiles.delete(element);
    if (!this.playback.has(participantId))
      this.playback.set(participantId, { volume: 1, muted: false, hidden: false, quality: 'auto' });
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
        tile
          .requestFullscreen()
          .catch(() => this.options.notify('Fullscreen is unavailable for this video.'));
      });
      fullscreen.dataset['control'] = 'fullscreen';
      fullscreen.hidden = typeof tile.requestFullscreen !== 'function';
      const pip = this.button('Picture in picture', () => {
        const video = tile.querySelector('video');
        if (!video || typeof video.requestPictureInPicture !== 'function') return;
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
      tile.append(details, hiddenNotice, blockedNotice);
      this.tiles.set(tile, {
        participantId,
        controls: details,
        blockedNotice,
        blocked: new Map(),
        playbackVersion: 0,
      });
    }
    this.applyParticipant(participantId);
    const state = this.playback.get(participantId)!;
    if (state.hidden) this.options.getRoom()?.setRemoteMediaHidden(participantId, true);
    if (state.quality !== 'auto')
      this.options.getRoom()?.setRemoteVideoQuality(participantId, state.quality);
  }

  detachParticipant(participantId: string): void {
    for (const [tile, info] of this.tiles) {
      if (info.participantId === participantId) {
        info.controls.remove();
        info.blockedNotice.remove();
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

function mediaErrorMessage(error: unknown): string {
  if (error instanceof Error) {
    if (error.name === 'NotAllowedError')
      return 'Camera or microphone permission was denied. Allow access in your browser and try again.';
    if (error.name === 'NotFoundError')
      return 'No matching camera or microphone was found. Select another device or preview only one device.';
    if (error.name === 'OverconstrainedError')
      return 'The selected device is unavailable. Choose a default device and try again.';
    if (error.name === 'NotReadableError')
      return 'The device could not be opened. Check whether another app is using it.';
    return error.message;
  }
  return 'Could not open your media devices. Please try again.';
}
