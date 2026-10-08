interface LifecycleRoom {
  readonly membershipVersion: number;
  readonly connected: boolean;
  setMediaPageActive(active: boolean): void;
  resumeMediaConnection(): void;
}

interface MediaLifecycleOptions {
  getRoom: () => LifecycleRoom | null;
  resumePlayback: () => void;
  refreshDevices: () => void;
  resumeSignaling: () => void;
  setPageActive: (active: boolean) => void;
}

/** Browser lifecycle hints never authorize capture, room entry, or a new speaker. */
export class MediaLifecycle {
  private timer: ReturnType<typeof setTimeout> | undefined;
  private disposed = false;
  private pageHidden = false;
  private pendingDevices = false;
  private lastRecovery = -Infinity;
  private readonly visibility = () => {
    this.pageHidden = document.visibilityState === 'hidden';
    this.options.getRoom()?.setMediaPageActive(!this.pageHidden);
    this.options.setPageActive(!this.pageHidden);
    if (this.pageHidden) this.cancel();
    else this.schedule();
  };
  private readonly hide = () => {
    this.pageHidden = true;
    this.options.getRoom()?.setMediaPageActive(false);
    this.options.setPageActive(false);
    this.cancel();
  };
  private readonly show = () => {
    this.pageHidden = document.visibilityState === 'hidden';
    this.options.getRoom()?.setMediaPageActive(!this.pageHidden);
    this.options.setPageActive(!this.pageHidden);
    this.schedule();
  };
  private readonly online = () => this.schedule();
  private readonly devices = () => {
    this.pendingDevices = true;
    this.schedule();
  };

  constructor(private readonly options: MediaLifecycleOptions) {
    document.addEventListener('visibilitychange', this.visibility);
    window.addEventListener('pagehide', this.hide);
    window.addEventListener('pageshow', this.show);
    window.addEventListener('online', this.online);
    navigator.mediaDevices?.addEventListener('devicechange', this.devices);
  }

  private schedule(): void {
    if (this.disposed || this.pageHidden || document.visibilityState === 'hidden') return;
    if (this.timer !== undefined) return;
    const room = this.options.getRoom();
    const membership = room?.membershipVersion;
    // A burst gets one recovery pass; successive hints cannot continuously
    // restart ICE. Online events are hints, never proof of server reachability.
    const delay = Math.max(250, this.lastRecovery + 3000 - Date.now());
    this.timer = setTimeout(() => {
      this.timer = undefined;
      if (this.disposed || this.pageHidden || document.visibilityState === 'hidden') return;
      if (room !== this.options.getRoom() || membership !== room?.membershipVersion) return;
      this.lastRecovery = Date.now();
      this.options.resumeSignaling();
      if (this.pendingDevices || room) {
        this.pendingDevices = false;
        this.options.refreshDevices();
      }
      if (!room) return;
      room.setMediaPageActive(true);
      this.options.resumePlayback();
      if (room.connected) room.resumeMediaConnection();
    }, delay);
  }

  private cancel(): void {
    clearTimeout(this.timer);
    this.timer = undefined;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.cancel();
    document.removeEventListener('visibilitychange', this.visibility);
    window.removeEventListener('pagehide', this.hide);
    window.removeEventListener('pageshow', this.show);
    window.removeEventListener('online', this.online);
    navigator.mediaDevices?.removeEventListener('devicechange', this.devices);
  }
}
