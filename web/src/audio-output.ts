/** Speaker routing is local to this tab and never opens a capture device. */
export class AudioOutput {
  private deviceId = '';
  private generation = 0;
  private jobs = new WeakMap<HTMLMediaElement, Promise<void>>();

  constructor(private readonly elements: () => HTMLMediaElement[]) {}

  get selected(): string {
    return this.deviceId;
  }

  get supported(): boolean {
    return (
      typeof HTMLMediaElement !== 'undefined' &&
      typeof HTMLMediaElement.prototype.setSinkId === 'function'
    );
  }

  async change(deviceId: string): Promise<void> {
    this.deviceId = deviceId;
    this.generation++;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        Promise.all(this.elements().map((element) => this.attach(element))),
        new Promise<never>((_, reject) => {
          timer = setTimeout(
            () => reject(new DOMException('Speaker change not confirmed', 'TimeoutError')),
            5000,
          );
        }),
      ]);
    } finally {
      clearTimeout(timer);
    }
  }

  attach(element: HTMLMediaElement): Promise<void> {
    if (typeof element.setSinkId !== 'function') return Promise.resolve();
    const pending = this.jobs.get(element);
    if (pending) return pending;
    const task = this.route(element).finally(() => this.jobs.delete(element));
    this.jobs.set(element, task);
    return task;
  }

  private async route(element: HTMLMediaElement): Promise<void> {
    // A native call cannot be cancelled. Keep one per element; a later choice
    // replaces desired state and runs only after the earlier native call settles.
    while (this.elements().includes(element)) {
      const generation = this.generation;
      if (element.sinkId === this.deviceId) return;
      try {
        await element.setSinkId(this.deviceId);
      } catch (error) {
        if (generation === this.generation) throw error;
      }
      if (generation === this.generation) return;
    }
  }
}

/** One native enumeration at a time; bursts coalesce and retired dialogs stay retired. */
export function observeMediaDevices(
  changed: (devices: MediaDeviceInfo[]) => void,
  failed: () => void,
): { refresh: () => void; dispose: () => void } {
  let disposed = false;
  let pending = false;
  let again = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const devices = navigator.mediaDevices;
  const refresh = (): void => {
    if (disposed) return;
    if (pending) {
      again = true;
      return;
    }
    pending = true;
    let expired = false;
    timer = setTimeout(() => {
      expired = true;
      if (!disposed) failed();
    }, 5000);
    Promise.resolve()
      .then(() => devices?.enumerateDevices() ?? [])
      .then(
        (result) => {
          if (!disposed && !expired) changed(result);
        },
        () => {
          if (!disposed && !expired) failed();
        },
      )
      .finally(() => {
        clearTimeout(timer);
        pending = false;
        if (again) {
          again = false;
          refresh();
        }
      })
      .catch(() => {
        /* A closed view must not create an unhandled rejection. */
      });
  };
  devices?.addEventListener('devicechange', refresh);
  refresh();
  return {
    refresh,
    dispose: () => {
      disposed = true;
      clearTimeout(timer);
      devices?.removeEventListener('devicechange', refresh);
    },
  };
}

/** A short generated tone; no microphone loopback, remote request, or persistent audio. */
export class SpeakerTest {
  readonly element: HTMLAudioElement;
  private readonly url: string;
  private timer: ReturnType<typeof setTimeout> | undefined;
  private disposed = false;

  constructor() {
    const rate = 8000;
    const samples = 3200;
    const bytes = new ArrayBuffer(44 + samples * 2);
    const view = new DataView(bytes);
    for (const [offset, value] of [
      [0, 'RIFF'],
      [8, 'WAVEfmt '],
      [36, 'data'],
    ] as const)
      for (let index = 0; index < value.length; index++)
        view.setUint8(offset + index, value.charCodeAt(index));
    view.setUint32(4, bytes.byteLength - 8, true);
    view.setUint32(16, 16, true);
    view.setUint16(20, 1, true);
    view.setUint16(22, 1, true);
    view.setUint32(24, rate, true);
    view.setUint32(28, rate * 2, true);
    view.setUint16(32, 2, true);
    view.setUint16(34, 16, true);
    view.setUint32(40, samples * 2, true);
    for (let index = 0; index < samples; index++) {
      const fade = Math.min(1, index / 80, (samples - index) / 80);
      view.setInt16(
        44 + index * 2,
        Math.sin((2 * Math.PI * 440 * index) / rate) * fade * 6000,
        true,
      );
    }
    this.url = URL.createObjectURL(new Blob([bytes], { type: 'audio/wav' }));
    try {
      this.element = new Audio(this.url);
    } catch (error) {
      URL.revokeObjectURL(this.url);
      throw error;
    }
  }

  play(): Promise<void> {
    if (this.disposed) return Promise.resolve();
    this.stop();
    // Called directly from a button to retain native user activation.
    const result = this.element.play();
    this.timer = setTimeout(() => this.stop(), 1000);
    return result.then(() => {
      if (this.disposed) this.element.pause();
    });
  }

  stop(): void {
    clearTimeout(this.timer);
    this.element.pause();
    this.element.currentTime = 0;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.stop();
    this.element.removeAttribute('src');
    this.element.load();
    URL.revokeObjectURL(this.url);
  }
}

export function outputErrorMessage(error: unknown): string {
  const name = error instanceof Error ? error.name : '';
  if (name === 'TimeoutError')
    return 'Speaker change is not confirmed. Try the default output or your system sound settings.';
  if (name === 'NotAllowedError')
    return 'Speaker selection was cancelled or blocked. Allow speaker access or use the default output.';
  if (name === 'NotFoundError')
    return 'That speaker is no longer available. Choose another output.';
  return 'Could not use that speaker. Choose the default output or check your system sound settings.';
}
