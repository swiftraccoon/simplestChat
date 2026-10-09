import { api, button, el } from './ui';

interface PwaOptions {
  getToken: () => string | null;
  getAccountId: () => string | null;
  openMessages: () => void;
  mountNotificationPreferences?: (container: HTMLElement, current: () => boolean) => void;
}

interface InstallPrompt extends Event {
  prompt(): Promise<{ outcome: 'accepted' | 'dismissed' }>;
}

interface PushOwner {
  accountId: string;
  lease: string;
}

const OWNER_KEY = 'simplestchat.pushOwner';
const LOCK_NAME = 'simplestchat.pushSubscription';

function readOwner(): PushOwner | null {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(OWNER_KEY) ?? 'null');
    if (
      value &&
      typeof value === 'object' &&
      'accountId' in value &&
      typeof value.accountId === 'string' &&
      'lease' in value &&
      typeof value.lease === 'string'
    )
      return { accountId: value.accountId, lease: value.lease };
  } catch {
    // Storage may be unavailable in a private or restricted browsing context.
  }
  return null;
}

function storeOwner(owner: PushOwner | null): void {
  // A durable nonsecret lease is required to distinguish a newer account/tab's
  // subscription from this tab's delayed logout cleanup. Never store tokens.
  if (owner) localStorage.setItem(OWNER_KEY, JSON.stringify(owner));
  else localStorage.removeItem(OWNER_KEY);
}

function appInstalled(): boolean {
  return (
    window.matchMedia('(display-mode: standalone)').matches ||
    (navigator as Navigator & { standalone?: boolean }).standalone === true
  );
}

function iosDevice(): boolean {
  return (
    /iPhone|iPad|iPod/.test(navigator.userAgent) ||
    (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1)
  );
}

function pushAvailable(): boolean {
  return (
    window.isSecureContext &&
    'serviceWorker' in navigator &&
    'PushManager' in window &&
    'Notification' in window &&
    'locks' in navigator &&
    (!iosDevice() || appInstalled())
  );
}

async function registerWorker(): Promise<ServiceWorkerRegistration> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      navigator.serviceWorker
        .register('/sw.js', { scope: '/', updateViaCache: 'none' })
        .then(() => navigator.serviceWorker.ready),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error('App setup timed out')), 10000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

/** Install and optional generic PM alerts; no offline cache or background credentials. */
export class PwaControls {
  private account: string | null;
  private generation = 0;
  private owner: PushOwner | null;
  private registration: Promise<ServiceWorkerRegistration> | null = null;
  private installPrompt: InstallPrompt | null = null;
  private openRequested = false;
  private busy = false;
  private refreshInstall: (() => void) | null = null;
  private refreshControls: (() => void) | null = null;
  private readonly controller = new AbortController();

  constructor(private readonly options: PwaOptions) {
    this.account = options.getAccountId();
    const owner = readOwner();
    this.owner = owner?.accountId === this.account ? owner : null;
    if (window.isSecureContext && 'serviceWorker' in navigator) {
      this.registration = registerWorker();
      // Registration is optional; opening the app cannot produce a rejection.
      this.registration.catch(() => {});
      navigator.serviceWorker.addEventListener(
        'message',
        (event: MessageEvent<unknown>) => {
          if (
            event.data &&
            typeof event.data === 'object' &&
            'type' in event.data &&
            event.data.type === 'openMessages'
          ) {
            this.openRequested = true;
            this.openPendingMessages();
          }
        },
        { signal: this.controller.signal },
      );
    }
    window.addEventListener(
      'beforeinstallprompt',
      (event) => {
        event.preventDefault();
        this.installPrompt = event as InstallPrompt;
        this.refreshInstall?.();
      },
      { signal: this.controller.signal },
    );
    window.addEventListener(
      'appinstalled',
      () => {
        this.installPrompt = null;
        this.refreshInstall?.();
      },
      { signal: this.controller.signal },
    );
    const url = new URL(window.location.href);
    if (url.searchParams.get('messages') === '1') {
      url.searchParams.delete('messages');
      window.history.replaceState(window.history.state, '', url);
      this.openRequested = true;
    }
    // Root construction may precede the Messages UI and account restoration.
    queueMicrotask(() => this.openPendingMessages());
  }

  private openPendingMessages(): void {
    if (
      this.controller.signal.aborted ||
      !this.openRequested ||
      !this.options.getToken() ||
      !this.options.getAccountId()
    )
      return;
    this.openRequested = false;
    this.options.openMessages();
  }

  /** Called after account restoration/change. Old leases never retire a newer tab's subscription. */
  accountChanged(): void {
    if (this.controller.signal.aborted) return;
    const next = this.options.getAccountId();
    if (next !== this.account) {
      const old = this.owner;
      this.generation++;
      this.account = next;
      const owner = readOwner();
      this.owner = owner?.accountId === next ? owner : null;
      if (old && navigator.locks && this.registration) {
        const registration = this.registration;
        navigator.locks
          .request(LOCK_NAME, async () => {
            if (readOwner()?.lease !== old.lease) return;
            const worker = await registration;
            if (readOwner()?.lease !== old.lease) return;
            const subscription = await worker.pushManager.getSubscription();
            if (readOwner()?.lease !== old.lease) return;
            await subscription?.unsubscribe();
            if (readOwner()?.lease === old.lease) storeOwner(null);
            worker.active?.postMessage({ type: 'clearNotifications' });
          })
          .catch(() => {
            // The server independently retires revoked/expired account sessions.
          });
      }
    }
    this.openPendingMessages();
  }

  /** Closing a tab keeps confirmed push subscriptions usable while the app is closed. */
  dispose(): void {
    this.generation++;
    this.controller.abort();
    this.refreshInstall = null;
    this.refreshControls = null;
  }

  mountAccount(container: HTMLElement, current: () => boolean): void {
    const section = el('section', undefined, 'community-dialog-body');
    section.append(el('h3', 'App & notifications'));
    const installHint = el('p', undefined, 'setting-hint');
    const install = button('Install app', () => {
      const prompt = this.installPrompt;
      if (!prompt || !current()) return;
      this.installPrompt = null;
      prompt
        .prompt()
        .catch(() => {})
        .finally(() => this.refreshInstall?.());
    });
    const refreshInstall = () => {
      if (!current()) return;
      install.hidden = appInstalled() || !this.installPrompt;
      installHint.textContent = appInstalled()
        ? 'Installed on this device. An internet connection is needed for chat and calls.'
        : iosDevice()
          ? 'On iPhone or iPad, use Share → Add to Home Screen, then open the installed app to enable notifications.'
          : 'Install from your browser’s app menu for a separate app window. Chat and calls need an internet connection.';
    };
    this.refreshInstall = refreshInstall;
    refreshInstall();
    const status = el('p', 'Checking notifications…', 'setting-hint');
    status.setAttribute('role', 'status');
    const explanation = el(
      'p',
      'Enable optional background PM alerts on this browser. Your account notification rules and conversation mutes also apply. Background notifications never show names or message text.',
      'setting-hint',
    );
    const account = this.options.getAccountId();
    const generation = this.generation;
    const isCurrent = () =>
      current() && account === this.options.getAccountId() && generation === this.generation;
    let enabled = false;
    let publicKey = '';
    let worker: ServiceWorkerRegistration | null = null;
    const toggle = button('Enable notifications', () => {
      if (!isCurrent() || this.busy || !worker || !publicKey) return;
      const token = this.options.getToken();
      if (!token || !account) return;
      this.busy = true;
      toggle.disabled = true;
      const disabling = enabled;
      // This permission request runs in the original button gesture, before
      // waiting for either a lock or network operation (required on iOS).
      let permission: Promise<NotificationPermission>;
      try {
        permission =
          disabling || Notification.permission === 'granted'
            ? Promise.resolve(Notification.permission)
            : Notification.requestPermission();
      } catch {
        permission = Promise.reject(new Error('Notification permission is unavailable'));
      }
      const registration = worker;
      const action = async () => {
        const allowed = await permission;
        if (!isCurrent()) return;
        if (!disabling && allowed !== 'granted') {
          status.textContent =
            'Notifications are blocked. Allow them in your browser or device settings, then try again.';
          return;
        }
        await navigator.locks.request(LOCK_NAME, async () => {
          if (!isCurrent()) return;
          let subscription = await registration.pushManager.getSubscription();
          if (!isCurrent()) return;
          if (disabling) {
            await api.disablePush(token, AbortSignal.timeout(10000));
            if (!isCurrent()) return;
            const owner = readOwner();
            if (!owner || owner.accountId === account) {
              await subscription?.unsubscribe();
              storeOwner(null);
              this.owner = null;
              registration.active?.postMessage({ type: 'clearNotifications' });
            }
            enabled = false;
            status.textContent = 'Private-message notifications are off for this browser session.';
          } else {
            const owner = readOwner();
            // A push endpoint is browser-wide. Do not bind an old account's
            // endpoint to a new one or silently adopt an unowned subscription.
            if (subscription && (!owner || owner.accountId !== account)) {
              if (!(await subscription.unsubscribe()))
                throw new Error('Previous notification subscription could not be retired');
              subscription = null;
            }
            if (!isCurrent()) return;
            const lease = { accountId: account, lease: crypto.randomUUID() };
            storeOwner(lease);
            this.owner = lease;
            subscription ??= await registration.pushManager.subscribe({
              userVisibleOnly: true,
              applicationServerKey: publicKey,
            });
            if (!isCurrent()) {
              await subscription.unsubscribe();
              if (readOwner()?.lease === lease.lease) storeOwner(null);
              return;
            }
            const cleanup = async () => {
              // The request may have committed even if the view/account was
              // retired before its response. Target only the original session.
              await api.disablePush(token, AbortSignal.timeout(10000)).catch(() => {});
              if (readOwner()?.lease !== lease.lease) return;
              await subscription?.unsubscribe();
              if (readOwner()?.lease === lease.lease) storeOwner(null);
              registration.active?.postMessage({ type: 'clearNotifications' });
            };
            try {
              await api.enablePush(
                token,
                { endpoint: subscription.endpoint },
                AbortSignal.timeout(10000),
              );
            } catch (error) {
              if (!isCurrent()) await cleanup();
              throw error;
            }
            if (!isCurrent()) {
              await cleanup();
              return;
            }
            enabled = true;
            if (isCurrent())
              status.textContent = 'Private-message notifications are on for this browser session.';
          }
        });
      };
      action()
        .catch(() => {
          if (isCurrent())
            status.textContent =
              'Notifications could not be updated. Check your connection and try again.';
        })
        .finally(() => {
          this.busy = false;
          this.refreshControls?.();
          if (isCurrent()) {
            toggle.disabled = false;
            toggle.textContent = enabled ? 'Disable notifications' : 'Enable notifications';
          }
        });
    });
    toggle.disabled = true;
    this.refreshControls = () => {
      if (isCurrent()) toggle.disabled = this.busy || !worker || !publicKey;
    };
    section.append(installHint, install, explanation, status, toggle);
    container.append(section);
    this.options.mountNotificationPreferences?.(container, isCurrent);
    if (!pushAvailable() || !this.registration) {
      status.textContent =
        iosDevice() && !appInstalled()
          ? 'Install the app on your Home Screen to enable notifications on this device.'
          : 'Background notifications are unavailable in this browser. Chat still works while it is open.';
      toggle.hidden = true;
      return;
    }
    const token = this.options.getToken();
    if (!token) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    Promise.race([
      Promise.all([this.registration, api.pushStatus(token, AbortSignal.timeout(10000))]),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error('Notification setup timed out')), 10000);
      }),
    ])
      .then(async ([registration, settings]) => {
        if (!isCurrent()) return;
        const subscription = await registration.pushManager.getSubscription();
        if (!isCurrent()) return;
        worker = registration;
        enabled = settings.enabled && !!subscription && readOwner()?.accountId === account;
        publicKey = settings.publicKey;
        toggle.disabled = this.busy || !publicKey;
        toggle.textContent = enabled ? 'Disable notifications' : 'Enable notifications';
        status.textContent = !publicKey
          ? 'Background notifications are not configured on this server.'
          : enabled
            ? 'Private-message notifications are on for this browser session.'
            : 'Private-message notifications are off.';
      })
      .catch(() => {
        if (isCurrent())
          status.textContent =
            'Notification settings could not be loaded. Close Account and try again.';
      })
      .finally(() => clearTimeout(timer));
  }
}
