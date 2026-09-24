interface RoomNavigationOptions {
  leave: () => Promise<void>;
  select: (id: string) => void;
  pending: (pending: boolean) => void;
  error: (error: unknown) => void;
}

const invalidRoomIdCharacter = /[^A-Za-z0-9_-]/;

/** Room links select a destination; joining and starting capture remain explicit. */
export class RoomNavigation {
  private selected = '';
  private destination = '';
  private observedHash = '';
  private joined = false;
  private leaving = false;
  private disposed = false;
  private intent = 0;

  constructor(private readonly options: RoomNavigationOptions) {
    const initial = this.readHash();
    if (initial !== null) this.selected = this.destination = initial;
    else this.writeHash('', false);
    this.observedHash = window.location.hash;
    window.addEventListener('hashchange', this.onLocationChange);
    window.addEventListener('popstate', this.onLocationChange);
    this.options.select(this.selected);
  }

  /** Record an explicit join attempt without causing another leave or selection. */
  join(id: string): boolean {
    if (this.disposed || this.leaving || !this.validate(id, false)) return false;
    if (!this.writeHash(id, true)) return false;
    this.intent++;
    this.selected = this.destination = id;
    this.joined = true;
    return true;
  }

  home(): void {
    this.selectRoom('');
  }

  selectRoom(id: string): void {
    if (this.disposed || !this.validate(id, true)) return;
    if (!this.writeHash(id, true)) return;
    this.requestSelection(id);
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.intent++;
    window.removeEventListener('hashchange', this.onLocationChange);
    window.removeEventListener('popstate', this.onLocationChange);
    if (this.leaving) this.options.pending(false);
  }

  private validate(id: string, allowHome: boolean): boolean {
    if (
      (allowHome && id === '') ||
      (id.length >= 1 && id.length <= 128 && !invalidRoomIdCharacter.test(id))
    )
      return true;
    this.options.error(
      new Error('Room links must use 1–128 letters, numbers, hyphens, or underscores.'),
    );
    return false;
  }

  private readHash(): string | null {
    const hash = window.location.hash.slice(1);
    // A valid encoded ID needs at most three characters per ASCII character.
    if (hash.length > 384) {
      this.options.error(new Error('This room link is too long.'));
      return null;
    }
    let id: string;
    try {
      id = decodeURIComponent(hash);
    } catch {
      this.options.error(new Error('This room link contains an invalid encoding.'));
      return null;
    }
    return this.validate(id, true) ? id : null;
  }

  private writeHash(id: string, push: boolean): boolean {
    const hash = id === '' ? '' : `#${id}`;
    try {
      if (window.location.hash !== hash) {
        const url = `${window.location.pathname}${window.location.search}${hash}`;
        if (push) window.history.pushState(null, '', url);
        else window.history.replaceState(null, '', url);
      }
      this.observedHash = window.location.hash;
      return true;
    } catch {
      this.options.error(
        new Error('The browser could not update this room link. Please try again.'),
      );
      return false;
    }
  }

  private readonly onLocationChange = (): void => {
    if (this.disposed || window.location.hash === this.observedHash) return;
    this.observedHash = window.location.hash;
    const id = this.readHash();
    if (id === null) {
      this.writeHash(this.destination, false);
      return;
    }
    this.requestSelection(id);
  };

  private requestSelection(id: string): void {
    if (id === this.destination) {
      if (this.leaving) return;
      if (!this.joined) {
        this.options.select(id);
        return;
      }
    }
    this.destination = id;
    this.intent++;
    if (this.leaving) return;
    this.leaving = true;
    this.options.pending(true);
    this.finishSelection().catch((error: unknown) => {
      if (!this.disposed) this.options.error(error);
    });
  }

  private async finishSelection(): Promise<void> {
    try {
      await this.options.leave();
    } catch (error: unknown) {
      if (this.disposed) return;
      this.destination = this.selected;
      this.writeHash(this.selected, false);
      this.leaving = false;
      this.options.pending(false);
      this.options.error(error);
      return;
    }
    if (this.disposed) return;
    const intent = this.intent;
    const id = this.destination;
    this.selected = id;
    this.joined = false;
    this.leaving = false;
    this.options.pending(false);
    if (!this.disposed && this.intent === intent) this.options.select(id);
  }
}
