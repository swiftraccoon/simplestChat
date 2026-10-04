import { avatarColors } from './avatar-colors';
import { button, el, safeRasterUrl } from './ui';

export interface ParticipantHovercardData {
  id: string;
  name: string;
  online: boolean;
  self: boolean;
  role?: string;
  color?: string;
  avatarUrl?: string;
  profileAvailable?: boolean;
  canMessage?: boolean;
  canMore?: boolean;
}

export interface ParticipantHovercardProfile {
  displayName?: string;
  bio?: string;
  avatarUrl?: string;
}

export interface ParticipantHovercardOptions {
  getSession: () => unknown;
  getParticipant: (id: string, fallbackName: string) => ParticipantHovercardData | null;
  loadProfile?: (id: string) => Promise<ParticipantHovercardProfile | null>;
  onMessage?: (id: string, name: string) => void;
  onProfile?: (id: string, name: string) => void;
  onMore?: (id: string, name: string, anchor: HTMLElement) => void;
}

/** A single, non-modal card shared by room names and historical chat names. */
export class ParticipantHovercard {
  private readonly card = el('div', undefined, 'participant-hovercard');
  private readonly events = new AbortController();
  private readonly anchorObserver = new MutationObserver(() => {
    if (this.anchor && !this.anchor.isConnected) this.close();
  });
  private anchor: HTMLElement | null = null;
  private openTimer: ReturnType<typeof setTimeout> | null = null;
  private closeTimer: ReturnType<typeof setTimeout> | null = null;
  private generation = 0;
  private pinned = false;
  private restoringFocus = false;
  private destroyed = false;
  private session: unknown;
  private participant: ParticipantHovercardData | null = null;

  constructor(private readonly options: ParticipantHovercardOptions) {
    this.card.id = 'participant-hovercard';
    this.card.hidden = true;
    this.card.tabIndex = -1;
    this.card.setAttribute('role', 'dialog');
    this.card.setAttribute('aria-modal', 'false');
    document.body.append(this.card);
    const events = { signal: this.events.signal };
    this.card.addEventListener('pointerenter', () => this.cancelClose(), events);
    this.card.addEventListener('pointerleave', () => this.scheduleClose(), events);
    this.card.addEventListener('focusin', () => this.cancelClose(), events);
    this.card.addEventListener('focusout', () => this.scheduleClose(true), events);
    this.card.addEventListener(
      'keydown',
      (event) => {
        if (event.key !== 'Tab') return;
        const actions = this.card.querySelectorAll<HTMLButtonElement>('button');
        const edge = event.shiftKey ? actions[0] : actions[actions.length - 1];
        if (document.activeElement !== edge && document.activeElement !== this.card) return;
        // The card is a portal: resume the trigger's document order at either edge.
        if (event.shiftKey) event.preventDefault();
        this.close(true);
      },
      events,
    );
    document.addEventListener(
      'pointerdown',
      (event) => {
        if (!this.contains(event.target)) this.close();
      },
      { ...events, capture: true },
    );
    document.addEventListener(
      'keydown',
      (event) => {
        if (event.key === 'Escape' && this.anchor) {
          event.preventDefault();
          event.stopPropagation();
          this.close(this.card.contains(document.activeElement));
        }
      },
      events,
    );
    document.addEventListener(
      'scroll',
      (event) => {
        if (!(event.target instanceof Node) || !this.card.contains(event.target)) this.close();
      },
      { ...events, capture: true, passive: true },
    );
    window.addEventListener('resize', () => this.close(), events);
    window.visualViewport?.addEventListener('resize', () => this.close(), events);
    window.visualViewport?.addEventListener('scroll', () => this.close(), events);
    document.addEventListener(
      'visibilitychange',
      () => {
        if (document.hidden) this.close();
      },
      events,
    );
  }

  /** Bind freshly rendered name buttons; their listeners leave with the button. */
  bind(anchor: HTMLElement, id: string, fallbackName: string): void {
    anchor.dataset['participantHovercard'] = id;
    anchor.setAttribute('aria-haspopup', 'dialog');
    anchor.setAttribute('aria-controls', this.card.id);
    anchor.setAttribute('aria-expanded', 'false');
    const open = () => this.open(anchor, id, fallbackName);
    anchor.addEventListener('pointerenter', (event) => {
      if (event.pointerType === 'touch') return;
      this.cancelOpen();
      this.cancelClose();
      const session = this.options.getSession();
      this.openTimer = setTimeout(() => {
        if (session === this.options.getSession()) open();
      }, 200);
    });
    anchor.addEventListener('pointerleave', () => {
      this.cancelOpen();
      this.scheduleClose();
    });
    anchor.addEventListener('focus', () => {
      if (!this.restoringFocus) open();
    });
    anchor.addEventListener('focusout', () => this.scheduleClose(true));
    anchor.addEventListener('click', (event) => {
      event.preventDefault();
      event.stopPropagation();
      open();
      this.pinned = true;
    });
    anchor.addEventListener('keydown', (event) => {
      if (event.key !== 'Tab' || event.shiftKey || this.anchor !== anchor) return;
      event.preventDefault();
      (this.card.querySelector<HTMLButtonElement>('button') ?? this.card).focus();
    });
  }

  close(restoreFocus = false): void {
    this.anchorObserver.disconnect();
    this.cancelOpen();
    this.cancelClose();
    this.generation++;
    const anchor = this.anchor;
    this.anchor = null;
    this.participant = null;
    this.pinned = false;
    this.card.hidden = true;
    this.card.replaceChildren();
    anchor?.setAttribute('aria-expanded', 'false');
    if (restoreFocus && anchor?.isConnected) {
      this.restoringFocus = true;
      anchor.focus({ preventScroll: true });
      this.restoringFocus = false;
    }
  }

  /** Call when leaving a room or changing identity, including while a profile loads. */
  reset(): void {
    this.close();
  }

  /** Roster updates must not leave a card attached to stale names or permissions. */
  refresh(): void {
    if (!this.anchor || !this.participant) return;
    const current = this.options.getParticipant(this.participant.id, this.participant.name);
    if (
      this.session !== this.options.getSession() ||
      !this.anchor.isConnected ||
      !current ||
      current.name !== this.participant.name ||
      current.online !== this.participant.online ||
      current.role !== this.participant.role ||
      current.canMessage !== this.participant.canMessage ||
      current.canMore !== this.participant.canMore ||
      current.profileAvailable !== this.participant.profileAvailable
    )
      this.close();
  }

  destroy(): void {
    this.close();
    this.destroyed = true;
    this.events.abort();
    this.card.remove();
  }

  private open(anchor: HTMLElement, id: string, fallbackName: string): void {
    this.cancelOpen();
    this.cancelClose();
    this.refresh();
    if (this.destroyed || !anchor.isConnected || (this.anchor === anchor && !this.card.hidden))
      return;
    const data = this.options.getParticipant(id, fallbackName);
    if (!data) return;
    this.close();
    this.anchor = anchor;
    this.participant = data;
    this.session = this.options.getSession();
    const session = this.session;
    const generation = this.generation;
    anchor.setAttribute('aria-expanded', 'true');
    this.card.setAttribute('aria-label', data.name);
    const avatar = el('div', [...data.name][0]?.toUpperCase(), 'participant-hovercard-avatar');
    avatar.setAttribute('aria-hidden', 'true');
    const colors = avatarColors(data.name, data.color);
    avatar.style.background = colors.background;
    avatar.style.color = colors.color;
    this.setAvatar(avatar, data.avatarUrl);
    const details = el('div', undefined, 'participant-hovercard-details');
    const profileName = el('p', undefined, 'participant-hovercard-profile-name');
    profileName.hidden = true;
    const status = [data.online ? 'In this room' : 'Offline', data.role]
      .filter(Boolean)
      .join(' · ');
    details.append(
      el('h3', data.name, 'participant-hovercard-name'),
      profileName,
      el('p', status, 'participant-hovercard-status'),
    );
    const header = el('div', undefined, 'participant-hovercard-header');
    header.append(avatar, details);
    const bio = el('p', undefined, 'participant-hovercard-bio');
    bio.hidden = true;
    const actions = el('div', undefined, 'participant-hovercard-actions');
    const action = (
      label: string,
      allowed: (current: ParticipantHovercardData) => boolean,
      callback: (current: ParticipantHovercardData) => void,
    ) => {
      actions.append(
        button(label, () => {
          const current = this.options.getParticipant(id, fallbackName);
          if (
            generation !== this.generation ||
            session !== this.options.getSession() ||
            !anchor.isConnected ||
            !current ||
            current.id !== id ||
            !allowed(current)
          ) {
            this.close();
            return;
          }
          this.close(true);
          callback(current);
        }),
      );
    };
    if (!data.self && data.canMessage && this.options.onMessage)
      action(
        'Message',
        (current) => !current.self && current.online && !!current.canMessage,
        (current) => this.options.onMessage?.(id, current.name),
      );
    if (data.profileAvailable && this.options.onProfile)
      action(
        'Profile',
        (current) => !!current.profileAvailable,
        (current) => this.options.onProfile?.(id, current.name),
      );
    if (data.canMore && this.options.onMore)
      action(
        'More',
        (current) => !!current.canMore,
        (current) => this.options.onMore?.(id, current.name, anchor),
      );
    this.card.append(header, bio);
    if (actions.childElementCount) this.card.append(actions);
    this.card.hidden = false;
    this.position();
    this.anchorObserver.observe(document.body, { childList: true, subtree: true });
    if (data.profileAvailable && this.options.loadProfile) {
      this.options
        .loadProfile(id)
        .then((profile) => {
          this.refresh();
          if (
            generation !== this.generation ||
            session !== this.options.getSession() ||
            !anchor.isConnected ||
            !profile
          )
            return;
          profileName.textContent = profile.displayName ?? '';
          profileName.hidden = !profile.displayName || profile.displayName === data.name;
          bio.textContent = profile.bio ?? '';
          bio.hidden = !profile.bio;
          this.setAvatar(avatar, profile.avatarUrl);
          this.position();
        })
        .catch(() => {
          // A profile is optional; the room identity and actions remain usable.
        });
    }
  }

  private setAvatar(avatar: HTMLElement, url?: string): void {
    if (!safeRasterUrl(url)) return;
    const image = el('img');
    image.alt = '';
    image.src = url;
    avatar.replaceChildren(image);
  }

  private position(): void {
    if (!this.anchor?.isConnected) {
      this.close();
      return;
    }
    const bounds = this.anchor.getBoundingClientRect();
    const viewport = window.visualViewport;
    const left = viewport?.offsetLeft ?? 0;
    const top = viewport?.offsetTop ?? 0;
    const width = viewport?.width ?? document.documentElement.clientWidth;
    const height = viewport?.height ?? window.innerHeight;
    this.card.style.maxWidth = `${Math.max(0, width - 16)}px`;
    this.card.style.maxHeight = `${Math.max(0, height - 16)}px`;
    const size = this.card.getBoundingClientRect();
    const x = Math.max(left + 8, Math.min(bounds.left, left + width - size.width - 8));
    const below = bounds.bottom + 6;
    const y = below + size.height <= top + height - 8 ? below : bounds.top - size.height - 6;
    this.card.style.left = `${x}px`;
    this.card.style.top = `${Math.max(top + 8, Math.min(y, top + height - size.height - 8))}px`;
  }

  private contains(target: EventTarget | null): boolean {
    return (
      target instanceof Node && (!!this.anchor?.contains(target) || this.card.contains(target))
    );
  }

  private scheduleClose(afterFocus = false): void {
    this.cancelClose();
    this.closeTimer = setTimeout(() => {
      if ((afterFocus || !this.pinned) && !this.contains(document.activeElement)) this.close();
    }, 180);
  }

  private cancelOpen(): void {
    if (this.openTimer !== null) clearTimeout(this.openTimer);
    this.openTimer = null;
  }

  private cancelClose(): void {
    if (this.closeTimer !== null) clearTimeout(this.closeTimer);
    this.closeTimer = null;
  }
}
