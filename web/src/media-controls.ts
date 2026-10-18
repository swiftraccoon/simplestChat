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
import {
  group,
  markSaved,
  navRow,
  note,
  selectRow,
  toggleRow,
  type ScreenContext,
  type SheetSection,
} from './settings-sheet';
import { AudioOutput, SpeakerTest, observeMediaDevices, outputErrorMessage } from './audio-output';
import {
  loadReceiveMode,
  loadFollowSpeaker,
  saveFollowSpeaker,
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
  setFollowSpeaker(enabled: boolean): void;
}

interface MediaControlsOptions {
  getRoom: () => MediaControlsRoom | null;
  notify: (message: string) => void;
  onPlaybackResult?: (element: HTMLMediaElement, blocked: boolean) => void;
  /** A row the application owns inside Camera & mic, such as the talk mode. */
  talkRow?: () => HTMLElement;
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

function make(tag: string, className = '', text = ''): HTMLElement {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

function actionButton(label: string, onClick: () => void): HTMLButtonElement {
  const node = make('button', '', label) as HTMLButtonElement;
  node.type = 'button';
  node.addEventListener('click', onClick);
  return node;
}
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
  /** Identifies the settings screen whose preview and device observer are live. */
  private setupToken: object | null = null;
  private readonly deviceLabels = new Map<string, string>();
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
            'Your selected speaker is not currently listed. Reconnect it or choose a speaker in Settings.',
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
    const followLabel = document.createElement('label');
    const follow = document.createElement('input');
    follow.type = 'checkbox';
    follow.setAttribute('aria-label', 'Follow speaker');
    follow.checked = loadFollowSpeaker();
    follow.title =
      'Prefer the current speaker’s camera. Pins, shared screens and picture-in-picture take priority. Tile sizes stay fixed.';
    follow.addEventListener('change', () => {
      saveFollowSpeaker(follow.checked);
      this.options.getRoom()?.setFollowSpeaker(follow.checked);
      for (const mounted of this.toolbars) {
        const control = mounted.querySelector<HTMLInputElement>('input[type="checkbox"]');
        if (control) control.checked = follow.checked;
      }
    });
    followLabel.append(follow, document.createTextNode('Follow speaker'));
    toolbar.append(followLabel);
    container.append(toolbar);
    this.toolbars.add(toolbar);
  }

  /** The settings sheet lists these screens; each applies a change as it is made. */
  sections(): SheetSection[] {
    return [
      {
        id: 'camera',
        segment: 'you',
        label: 'Camera & mic',
        status: () => this.captureStatus(),
        screen: () => ({
          title: 'Camera & mic',
          body: (context) => this.cameraScreen(context),
          leave: () => this.leaveSetup(),
        }),
      },
      {
        id: 'speaker',
        segment: 'you',
        label: 'Speaker',
        status: () => this.speakerStatus(),
        screen: () => ({
          title: 'Speaker',
          body: (context) => this.speakerScreen(context),
          leave: () => this.leaveSetup(),
        }),
      },
    ];
  }

  private deviceName(id: string, kind: string): string {
    return id ? (this.deviceLabels.get(id) ?? `Chosen ${kind}`) : `Default ${kind}`;
  }

  private captureStatus(): string {
    const preferences = loadCapturePreferences();
    return `${this.deviceName(preferences.cameraDeviceId, 'camera')} · ${this.deviceName(
      preferences.microphoneDeviceId,
      'mic',
    )}`;
  }

  private speakerStatus(): string {
    if (!this.output.supported) return 'System speaker';
    return this.output.selected
      ? (this.deviceLabels.get(this.output.selected) ?? 'Chosen speaker')
      : 'System default';
  }

  private rememberDevices(devices: MediaDeviceInfo[]): void {
    for (const device of devices)
      if (device.deviceId && device.label) this.deviceLabels.set(device.deviceId, device.label);
  }

  /** Fills a select with one device kind, keeping a saved device that is not listed. */
  private fillSelect(
    select: HTMLSelectElement,
    devices: MediaDeviceInfo[],
    kind: MediaDeviceKind,
    title: string,
    defaultLabel: string,
  ): void {
    const selected = select.value;
    select.replaceChildren(new Option(defaultLabel, ''));
    const matching = devices.filter((device) => device.kind === kind && device.deviceId);
    for (const [index, device] of matching.entries())
      select.add(new Option(device.label || `${title} ${index + 1}`, device.deviceId));
    if (selected && !matching.some((device) => device.deviceId === selected)) {
      // Device names/IDs may be concealed until the user grants preview permission.
      select.add(new Option(`Saved ${title} (not currently listed)`, selected));
    }
    select.value = selected;
  }

  /** Opens without capture. A change may switch an active device, but never enables an inactive one. */
  private cameraScreen(context: ScreenContext): HTMLElement {
    this.leaveSetup();
    const token = {};
    this.setupToken = token;
    const current = (): boolean => this.setupToken === token && context.current();
    let preferences = loadCapturePreferences();

    const previewBlock = make('div', 'sheet-block media-preview-panel');
    const video = make('video', 'media-preview-video') as HTMLVideoElement;
    video.autoplay = true;
    video.muted = true;
    video.playsInline = true;
    video.hidden = true;
    const meter = make('meter', 'media-level') as HTMLMeterElement;
    meter.min = 0;
    meter.max = 1;
    meter.value = 0;
    meter.setAttribute('aria-label', 'Microphone input level');
    const meterRow = make('div', 'media-meter-row');
    meterRow.append(make('span', 'media-meter-label', 'Mic level'), meter);
    const status = make('p', 'media-preview-status sheet-status', 'Previews stay on this device.');
    status.setAttribute('role', 'status');
    const cameraTest = actionButton('Test camera', () => startPreview(true, false));
    const microphoneTest = actionButton('Test microphone', () => startPreview(false, true));
    const stopButton = actionButton('Stop preview', () => stopPreview());
    stopButton.disabled = true;
    const actions = make('div', 'sheet-actions');
    actions.append(cameraTest, microphoneTest, stopButton);
    previewBlock.append(video, meterRow, actions, status);

    const cameraSelect = make('select') as HTMLSelectElement;
    cameraSelect.name = 'cameraDeviceId';
    cameraSelect.setAttribute('name', 'cameraDeviceId');
    const microphoneSelect = make('select') as HTMLSelectElement;
    microphoneSelect.name = 'microphoneDeviceId';
    microphoneSelect.setAttribute('name', 'microphoneDeviceId');
    cameraSelect.add(new Option('Default camera', ''));
    microphoneSelect.add(new Option('Default microphone', ''));
    for (const [select, name, title] of [
      [cameraSelect, 'cameraDeviceId', 'camera'],
      [microphoneSelect, 'microphoneDeviceId', 'microphone'],
    ] as const) {
      if (preferences[name]) select.add(new Option(`Saved ${title}`, preferences[name]));
      select.value = preferences[name];
    }
    const cameraRow = selectRow('Camera', cameraSelect);
    const microphoneRow = selectRow('Microphone', microphoneSelect);

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
    let previewAction = 0;
    let applying = false;
    const setBusy = (): void => {
      const busy = applying || preview.pending;
      cameraTest.disabled = microphoneTest.disabled = busy;
      cameraSelect.disabled = microphoneSelect.disabled = busy;
    };
    const stopPreview = (): void => {
      previewAction++;
      preview.stop();
      stopButton.disabled = true;
      setBusy();
      if (preview.pending)
        status.textContent = 'Preview cancelled. Dismiss the browser permission prompt first.';
    };
    const startPreview = (camera: boolean, microphone: boolean): void => {
      if (!current() || applying || preview.pending) return;
      const action = ++previewAction;
      const device = camera ? 'Camera' : 'Microphone';
      const start = async (): Promise<void> => {
        stopButton.disabled = false;
        status.textContent = `${device}: waiting for browser permission…`;
        try {
          // Start within this click's turn, requesting only the chosen kind.
          const capture = preview.start(preferences, camera, microphone);
          setBusy();
          const started = await capture;
          if (!current() || action !== previewAction || !started) return;
          status.textContent = camera
            ? 'Camera preview is live. Only you see it.'
            : 'Microphone test is live. Only you see the level; nothing plays back.';
          this.devices?.refresh();
        } catch (error) {
          if (!current() || action !== previewAction) return;
          stopButton.disabled = true;
          status.textContent = mediaErrorMessage(error, device);
        } finally {
          if (current()) setBusy();
        }
      };
      start().catch((error) => {
        if (current() && action === previewAction)
          status.textContent = mediaErrorMessage(error, device);
      });
    };

    const readPreferences = (): CapturePreferences =>
      normalizeCapturePreferences({
        ...preferences,
        cameraDeviceId: cameraSelect.value,
        microphoneDeviceId: microphoneSelect.value,
      });
    const apply = async (
      row: HTMLElement,
      select: HTMLSelectElement,
      name: 'cameraDeviceId' | 'microphoneDeviceId',
    ): Promise<void> => {
      if (applying) return;
      stopPreview();
      const next = readPreferences();
      const activeRoom = this.options.getRoom();
      const isCurrent = (): boolean => current() && this.options.getRoom() === activeRoom;
      applying = true;
      setBusy();
      try {
        if (!(await applyCaptureSettings(activeRoom, next, preferences, isCurrent))) return;
        preferences = loadCapturePreferences();
        status.textContent = '';
        markSaved(row);
      } catch (error) {
        if (!isCurrent()) return;
        // The confirmed baseline is what the select shows, so a retry stays possible.
        preferences = loadCapturePreferences();
        select.value = preferences[name];
        status.textContent = mediaErrorMessage(error);
      } finally {
        if (current()) {
          applying = false;
          setBusy();
        }
      }
    };
    cameraSelect.addEventListener('change', () => {
      apply(cameraRow, cameraSelect, 'cameraDeviceId').catch(() => {});
    });
    microphoneSelect.addEventListener('change', () => {
      apply(microphoneRow, microphoneSelect, 'microphoneDeviceId').catch(() => {});
    });

    this.devices = observeMediaDevices(
      (devices) => {
        if (!current()) return;
        this.rememberDevices(devices);
        this.fillSelect(cameraSelect, devices, 'videoinput', 'camera', 'Default camera');
        this.fillSelect(
          microphoneSelect,
          devices,
          'audioinput',
          'microphone',
          'Default microphone',
        );
      },
      () => {
        if (current()) status.textContent = 'Device list unavailable. The defaults still work.';
      },
    );

    const qualityStatus = (): string => {
      const saved = loadCapturePreferences();
      return `${saved.resolution} · ${saved.frameRate} fps`;
    };
    const qualityRow = navRow('Quality', qualityStatus(), () =>
      context.push(
        {
          title: 'Quality',
          body: (inner) =>
            this.qualityScreen(inner, () => {
              const shown = qualityRow.querySelector<HTMLElement>('.sheet-row-status');
              if (shown) shown.textContent = qualityStatus();
            }),
        },
        qualityRow,
      ),
    );
    const rows: HTMLElement[] = [cameraRow, microphoneRow];
    if (this.options.talkRow) rows.push(this.options.talkRow());
    rows.push(qualityRow);
    const body = make('div', 'media-setup');
    body.append(
      previewBlock,
      group(null, rows),
      note('Changes apply at once. A device that is off stays off.'),
    );
    return body;
  }

  /** Capture options that rarely change; each applies on its own. */
  private qualityScreen(context: ScreenContext, changed: () => void): HTMLElement {
    let preferences = loadCapturePreferences();
    const status = make('p', 'sheet-status');
    status.setAttribute('role', 'status');
    const resolution = make('select') as HTMLSelectElement;
    resolution.name = 'resolution';
    resolution.setAttribute('name', 'resolution');
    for (const [value, label] of [
      ['360p', '360p'],
      ['720p', '720p'],
      ['1080p', '1080p'],
    ])
      resolution.add(new Option(label, value));
    resolution.value = preferences.resolution;
    const frameRate = make('select') as HTMLSelectElement;
    frameRate.name = 'frameRate';
    frameRate.setAttribute('name', 'frameRate');
    for (const value of [15, 30, 60]) frameRate.add(new Option(`${value} fps`, String(value)));
    frameRate.value = String(preferences.frameRate);
    const resolutionRow = selectRow('Resolution', resolution);
    const frameRow = selectRow('Frame rate', frameRate);
    const toggles = {} as Record<
      'echoCancellation' | 'noiseSuppression' | 'autoGainControl',
      ReturnType<typeof toggleRow>
    >;
    const read = (): CapturePreferences =>
      normalizeCapturePreferences({
        ...preferences,
        resolution: resolution.value,
        frameRate: Number(frameRate.value),
        echoCancellation: toggles.echoCancellation.input.checked,
        noiseSuppression: toggles.noiseSuppression.input.checked,
        autoGainControl: toggles.autoGainControl.input.checked,
      });
    const restore = (): void => {
      resolution.value = preferences.resolution;
      frameRate.value = String(preferences.frameRate);
      for (const name of ['echoCancellation', 'noiseSuppression', 'autoGainControl'] as const)
        toggles[name].input.checked = preferences[name];
    };
    let applying = false;
    const apply = async (row: HTMLElement): Promise<void> => {
      if (applying) {
        restore();
        return;
      }
      const next = read();
      const activeRoom = this.options.getRoom();
      const isCurrent = (): boolean => context.current() && this.options.getRoom() === activeRoom;
      applying = true;
      try {
        if (!(await applyCaptureSettings(activeRoom, next, preferences, isCurrent))) return;
        preferences = loadCapturePreferences();
        status.textContent = '';
        markSaved(row);
        changed();
      } catch (error) {
        if (!isCurrent()) return;
        preferences = loadCapturePreferences();
        restore();
        status.textContent = mediaErrorMessage(error);
      } finally {
        if (context.current()) applying = false;
      }
    };
    for (const [name, label] of [
      ['echoCancellation', 'Echo cancellation'],
      ['noiseSuppression', 'Noise suppression'],
      ['autoGainControl', 'Automatic level'],
    ] as const) {
      const toggle = toggleRow(label, preferences[name], () => {
        apply(toggle.row).catch(() => {});
      });
      toggle.input.name = name;
      toggle.input.setAttribute('name', name);
      toggles[name] = toggle;
    }
    resolution.addEventListener('change', () => {
      apply(resolutionRow).catch(() => {});
    });
    frameRate.addEventListener('change', () => {
      apply(frameRow).catch(() => {});
    });
    return group(null, [
      resolutionRow,
      frameRow,
      toggles.echoCancellation.row,
      toggles.noiseSuppression.row,
      toggles.autoGainControl.row,
      note('Higher quality uses more bandwidth. The defaults suit most connections.'),
      status,
    ]);
  }

  /** Speaker choice applies to this tab at once; a test tone checks it. */
  private speakerScreen(context: ScreenContext): HTMLElement {
    this.leaveSetup();
    const token = {};
    this.setupToken = token;
    const current = (): boolean => this.setupToken === token && context.current();
    const output = make('select') as HTMLSelectElement;
    output.setAttribute('data-output-device', '');
    output.setAttribute('aria-label', 'Speaker');
    output.add(new Option('System default', ''));
    const outputStatus = make('p', 'sheet-status');
    outputStatus.setAttribute('data-output-status', '');
    outputStatus.setAttribute('role', 'status');
    const row = selectRow('Speaker', output);
    const picker = navigator.mediaDevices as MediaDevices & {
      selectAudioOutput?: () => Promise<MediaDeviceInfo>;
    };
    const outputChoose = actionButton('Choose another speaker', () => outputActionStart());
    const outputTest = actionButton('Test speaker', () => {
      const test = this.speakerTest;
      if (!test) return;
      test
        .play()
        .then(() => {
          if (current())
            outputStatus.textContent =
              'Test tone played. If you did not hear it, check the speaker and system volume.';
        })
        .catch((error) => {
          if (current()) outputStatus.textContent = outputErrorMessage(error);
        });
    });
    output.disabled = !this.output.supported;
    outputChoose.hidden = !this.output.supported || typeof picker?.selectAudioOutput !== 'function';
    if (!this.output.supported)
      outputStatus.textContent =
        'This browser uses your system speaker. Change output in your system sound settings.';
    if (this.output.selected) output.add(new Option('Selected speaker', this.output.selected));
    output.value = this.output.selected;
    let outputAction = 0;
    const applyOutput = async (deviceId: string, action: number): Promise<void> => {
      if (!current() || action !== outputAction) return;
      await this.output.change(deviceId);
      if (!current() || action !== outputAction) return;
      this.outputWarning = false;
      for (const id of this.playback.keys()) this.applyParticipant(id);
      outputStatus.textContent = 'Speaker selected for this tab. Use Test speaker to check it.';
      markSaved(row);
    };
    output.addEventListener('change', () => {
      const action = ++outputAction;
      this.speakerTest?.stop();
      applyOutput(output.value, action).catch((error) => {
        if (current() && action === outputAction)
          outputStatus.textContent = outputErrorMessage(error);
      });
    });
    const outputActionStart = (): void => {
      const action = ++outputAction;
      this.speakerTest?.stop();
      outputChoose.disabled = outputTest.disabled = output.disabled = true;
      outputStatus.textContent = 'Choose a speaker in your browser…';
      // The picker is invoked before any await, preserving this button's gesture.
      let selected: Promise<MediaDeviceInfo | null>;
      try {
        selected = picker.selectAudioOutput ? picker.selectAudioOutput() : Promise.resolve(null);
      } catch (error) {
        selected = Promise.reject(
          error instanceof Error ? error : new Error('Speaker selection failed'),
        );
      }
      selected
        .then(async (device) => {
          if (!current() || action !== outputAction) return;
          if (device) {
            output.add(new Option(device.label || 'Selected speaker', device.deviceId));
            output.value = device.deviceId;
          }
          await applyOutput(output.value, action);
          this.devices?.refresh();
        })
        .catch((error) => {
          if (current() && action === outputAction)
            outputStatus.textContent = outputErrorMessage(error);
        })
        .finally(() => {
          if (current() && action === outputAction) {
            outputChoose.disabled = outputTest.disabled = false;
            output.disabled = !this.output.supported;
          }
        });
    };
    this.devices = observeMediaDevices(
      (devices) => {
        if (!current()) return;
        this.rememberDevices(devices);
        const selected = output.value;
        output.replaceChildren(new Option('System default', ''));
        const speakers = devices.filter(
          (device) => device.kind === 'audiooutput' && device.deviceId,
        );
        for (const [index, device] of speakers.entries())
          output.add(new Option(device.label || `Speaker ${index + 1}`, device.deviceId));
        if (selected && !speakers.some((device) => device.deviceId === selected)) {
          output.add(new Option('Selected speaker (not currently listed)', selected));
          if (selected === this.output.selected)
            outputStatus.textContent =
              'Selected speaker is not currently listed. Choose another output or use the system default.';
        }
        output.value = selected;
      },
      () => {
        if (current()) outputStatus.textContent = 'Device list unavailable.';
      },
    );
    try {
      this.speakerTest = new SpeakerTest();
      outputTest.disabled = true;
      this.output
        .attach(this.speakerTest.element)
        .then(() => {
          if (current()) outputTest.disabled = false;
        })
        .catch((error) => {
          if (current()) outputStatus.textContent = outputErrorMessage(error);
        });
    } catch {
      outputTest.disabled = true;
      outputStatus.textContent = 'Speaker testing is unavailable in this browser.';
    }
    const actions = make('div', 'sheet-actions');
    actions.append(outputChoose, outputTest);
    return group(null, [row, actions, outputStatus]);
  }

  /** Stops the preview, the test tone and the device observer of the screen that was left. */
  private leaveSetup(): void {
    this.setupToken = null;
    this.devices?.dispose();
    this.devices = null;
    this.speakerTest?.dispose();
    this.speakerTest = null;
    this.preview?.stop();
    this.preview = null;
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
    this.leaveSetup();
    for (const id of this.playback.keys()) this.detachParticipant(id);
  }

  destroy(): void {
    if (this.stallTimer !== null) clearInterval(this.stallTimer);
    this.stallTimer = null;
    this.reset();
    for (const toolbar of this.toolbars) toolbar.remove();
    this.toolbars.clear();
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
