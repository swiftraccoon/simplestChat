import type { AuthResponse, UserInfo } from './protocol';
import type { TelemetryHandler, TelemetryOutcome } from './telemetry-types';

type AuthChangeHandler = (loggedIn: boolean, tokenRefresh: boolean) => void;

const RESTORE_DEADLINE_MS = 10_000;
const SESSION_DEADLINE_MS = 20_000;

/** A request may have created an HttpOnly cookie even when its response is lost. */
export class SessionOutcomeUnknownError extends Error {
  constructor() {
    super(
      'The server may have signed you in, but its response could not be confirmed. Reload to check your session before trying again.',
    );
    this.name = 'SessionOutcomeUnknownError';
  }
}

class AuthHttpRejection extends Error {}

const REFRESH_LOCK_NAME = 'simplestchat-refresh-v1';
// The server rejects, but does not revoke, the exact predecessor for two
// seconds so simultaneous tabs do not destroy the winning refresh. Retrying
// after that grace either uses the shared successor cookie or revokes a
// successor created on another device with a stolen token.
const REFRESH_REPLAY_CONFIRM_DELAY_MS = 2_250;
const REFRESH_RETRY_WINDOW_MS = 15_000;
const REFRESH_MAX_ATTEMPTS = 3;
const REFRESH_RETRY_DELAY_MS = 3_000;
const REFRESH_RETRY_JITTER_MS = 250;

interface RefreshBudget {
  deadline: number;
  wallDeadline: number;
  attempts: number;
  nextAttemptAt: number;
  expiryTimer: ReturnType<typeof setTimeout> | null;
}

interface ScheduledRefresh {
  generation: number;
  budget: RefreshBudget;
  controller: AbortController;
}

export class AuthManager {
  private _token: string | null = null;
  private _user: UserInfo | null = null;
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
  private refreshDeadline = 0;
  private tokenExpiresAt: number | null = null;
  private tokenExpiryDeadline: number | null = null;
  private refreshBudget: RefreshBudget | null = null;
  private scheduledRefresh: ScheduledRefresh | null = null;
  private onChange: AuthChangeHandler | null = null;
  private registrationCeremonyId: string | null = null;
  private authenticationCeremonyId: string | null = null;
  private generation = 0;
  private uncertainSession = false;
  private restoreController: AbortController | null = null;
  private sessionController: AbortController | null = null;
  private sessionMutation: (() => void) | null = null;
  private challengeGeneration: number | null = null;
  private telemetry: TelemetryHandler | undefined;

  /** Notify other tabs that their shared HttpOnly cookie may have changed.
   * The callback carries no identity, token or credential. */
  setSessionMutationHandler(handler: (() => void) | null): void {
    this.sessionMutation = handler;
  }

  private notifySessionMutation(): void {
    try {
      this.sessionMutation?.();
    } catch {
      /* Browser coordination is optional; the server remains authoritative. */
    }
  }

  setTelemetryHandler(handler: TelemetryHandler): void {
    this.telemetry = (event) => {
      try {
        handler(event);
      } catch {
        /* Telemetry cannot interrupt authentication or signaling. */
      }
    };
  }

  /** Retire challenge creation/ceremony before any session-creating request. */
  cancelPasskeyAttempt(): void {
    const generation = this.beginAuthentication();
    this.resumeRefresh(generation);
  }

  get isLoggedIn(): boolean {
    return this._token !== null;
  }

  get displayName(): string | null {
    return this._user?.display_name ?? null;
  }

  get userId(): string | null {
    return this._user?.id ?? null;
  }

  get jwt(): string | null {
    return this._token;
  }

  updateDisplayName(displayName: string): void {
    if (!this._user) return;
    this._user.display_name = displayName;
    this.onChange?.(true, true);
  }

  /** Clear local identity after a server-side password change revoked all sessions. */
  forgetSession(): void {
    this.clearSession();
    this.notifySessionMutation();
  }

  setOnChange(handler: AuthChangeHandler): void {
    this.onChange = handler;
  }

  /** Try to restore session from refresh token cookie on page load */
  async tryRestore(): Promise<boolean> {
    return this.restoreSession(false);
  }

  /** Revalidate after a cross-tab cookie change. Retire stale ceremonies and
   * requests; only an authoritative response can retain the current identity.
   * Failure clears this tab without broadcasting another invalidation. */
  async reconcileSharedSession(): Promise<void> {
    await this.restoreSession(true);
  }

  /** Synchronous fence for a newer external cookie hint while reconciliation
   * is already pending. The coordinator follows this with a bounded restore. */
  invalidateSharedSession(): void {
    this.beginAuthentication();
  }

  get canCheckSharedSession(): boolean {
    return (
      this.sessionController === null &&
      this.restoreController === null &&
      this.challengeGeneration === null &&
      this.registrationCeremonyId === null &&
      this.authenticationCeremonyId === null
    );
  }

  private async restoreSession(shared: boolean): Promise<boolean> {
    const generation = this.beginAuthentication();
    const started = performance.now();
    const deadline = started + RESTORE_DEADLINE_MS;
    const wallDeadline = Date.now() + RESTORE_DEADLINE_MS;
    const controller = new AbortController();
    this.restoreController = controller;
    let outcome: TelemetryOutcome = 'error';
    this.telemetry?.({ name: 'auth_restore', outcome: 'started' });
    const assertLive = (): void => {
      this.assertCurrent(generation);
      if (controller.signal.aborted || performance.now() >= deadline || Date.now() >= wallDeadline)
        throw new DOMException('Authentication restoration timed out', 'TimeoutError');
    };
    const timer = setTimeout(() => controller.abort(), RESTORE_DEADLINE_MS);
    try {
      // Race the entire operation, including Web Locks, JSON bodies and replay
      // delays. Abort alone is insufficient for a stalled/late platform promise.
      const work = async (): Promise<boolean> => {
        const resp = await this.requestRefreshWithReplayConfirmation(
          generation,
          undefined,
          controller.signal,
          assertLive,
        );
        assertLive();
        if (!resp.ok) {
          outcome = resp.status === 401 ? 'unauthenticated' : 'error';
          if (shared) {
            if (resp.status === 401) this.uncertainSession = false;
            this.restoreController = null;
            this.clearSession();
          }
          return false;
        }
        const data = await readSession(resp);
        assertLive();
        this.uncertainSession = false;
        if (this.restoreController === controller) this.restoreController = null;
        this.setSession(data, shared);
        outcome = 'ok';
        return true;
      };
      return await abortable(work(), controller.signal);
    } catch {
      outcome =
        generation !== this.generation
          ? 'superseded'
          : controller.signal.aborted || performance.now() >= deadline || Date.now() >= wallDeadline
            ? 'timeout'
            : 'error';
      if (shared && generation === this.generation) this.clearSession();
      return false;
    } finally {
      clearTimeout(timer);
      if (this.restoreController === controller) this.restoreController = null;
      controller.abort();
      this.telemetry?.({ name: 'auth_restore', outcome, durationMs: performance.now() - started });
      this.resumeRefresh(generation);
    }
  }

  async register(email: string, displayName: string, password: string): Promise<void> {
    await this.establishSession(
      '/api/auth/register',
      { email, password, display_name: displayName },
      'Registration failed',
    );
  }

  async login(email: string, password: string): Promise<void> {
    await this.establishSession('/api/auth/login', { email, password }, 'Login failed');
  }

  async passkeyRegisterStart(
    email: string,
    displayName: string,
    signal?: AbortSignal,
  ): Promise<CredentialCreationOptions> {
    if (this.uncertainSession) throw new SessionOutcomeUnknownError();
    const generation = this.beginAuthentication();
    this.challengeGeneration = generation;
    try {
      const options = record(
        await this.requestJson(
          '/api/auth/passkey/register/start',
          { email, display_name: displayName },
          'Passkey registration failed',
          generation,
          signal,
        ),
      );
      this.assertCurrent(generation);
      const ceremonyId = string(options['ceremony_id']);
      const result = deserializeCreationOptions(options);
      this.registrationCeremonyId = ceremonyId;
      return result;
    } finally {
      if (this.challengeGeneration === generation) this.challengeGeneration = null;
      this.resumeRefresh(generation);
    }
  }

  async passkeyRegisterFinish(credential: Credential): Promise<void> {
    const ceremonyId = this.registrationCeremonyId;
    this.registrationCeremonyId = null;
    if (!ceremonyId) throw new Error('Passkey registration was not started');
    await this.establishSession(
      '/api/auth/passkey/register/finish',
      { ceremony_id: ceremonyId, credential: serializeCredential(credential) },
      'Passkey registration failed',
    );
  }

  async passkeyLoginStart(signal?: AbortSignal): Promise<CredentialRequestOptions> {
    if (this.uncertainSession) throw new SessionOutcomeUnknownError();
    const generation = this.beginAuthentication();
    this.challengeGeneration = generation;
    try {
      const options = record(
        await this.requestJson(
          '/api/auth/passkey/login/start',
          {},
          'Passkey login failed',
          generation,
          signal,
        ),
      );
      this.assertCurrent(generation);
      const ceremonyId = string(options['ceremony_id']);
      const result = deserializeRequestOptions(options);
      this.authenticationCeremonyId = ceremonyId;
      return result;
    } finally {
      if (this.challengeGeneration === generation) this.challengeGeneration = null;
      this.resumeRefresh(generation);
    }
  }

  async passkeyLoginFinish(credential: Credential): Promise<void> {
    const ceremonyId = this.authenticationCeremonyId;
    this.authenticationCeremonyId = null;
    if (!ceremonyId) throw new Error('Passkey login was not started');
    await this.establishSession(
      '/api/auth/passkey/login/finish',
      { ceremony_id: ceremonyId, credential: serializeCredential(credential) },
      'Passkey login failed',
    );
  }

  async logout(): Promise<void> {
    const generation = this.beginAuthentication();
    const controller = new AbortController();
    this.sessionController = controller;
    const deadline = performance.now() + SESSION_DEADLINE_MS;
    const wallDeadline = Date.now() + SESSION_DEADLINE_MS;
    const timer = setTimeout(() => controller.abort(), SESSION_DEADLINE_MS);
    let submitted = false;
    try {
      await abortable(
        this.withSessionLock(controller.signal, async () => {
          this.assertCurrent(generation);
          if (
            controller.signal.aborted ||
            performance.now() >= deadline ||
            Date.now() >= wallDeadline
          )
            throw new Error('Sign out timed out; please check your session again');
          let response: Response;
          submitted = true;
          try {
            response = await fetch('/api/auth/logout', {
              method: 'POST',
              credentials: 'include',
              signal: controller.signal,
            });
          } catch (error) {
            this.notifySessionMutation();
            throw error;
          }
          if (response.ok) this.notifySessionMutation();
          this.assertCurrent(generation);
          if (!response.ok)
            throw new Error('Sign out could not revoke the session; please try again');
          if (this.sessionController === controller) this.sessionController = null;
          this.clearSession();
        }),
        controller.signal,
      );
    } catch (error) {
      if (submitted && controller.signal.aborted) this.notifySessionMutation();
      this.assertCurrent(generation);
      throw error;
    } finally {
      clearTimeout(timer);
      if (this.sessionController === controller) this.sessionController = null;
      controller.abort();
      this.resumeRefresh(generation);
    }
  }

  // Only the latest local authentication intent may adopt or retire identity.
  // This does not cancel server-side effects of requests already sent.
  private beginAuthentication(): number {
    this.generation += 1;
    this.restoreController?.abort();
    this.restoreController = null;
    this.sessionController?.abort();
    this.sessionController = null;
    if (this.refreshTimer !== null) clearTimeout(this.refreshTimer);
    this.refreshTimer = null;
    this.stopScheduledRefresh();
    this.registrationCeremonyId = null;
    this.authenticationCeremonyId = null;
    this.challengeGeneration = null;
    return this.generation;
  }

  private assertCurrent(generation: number): void {
    if (generation !== this.generation) throw new Error('Authentication request superseded');
  }

  private resumeRefresh(generation: number): void {
    if (generation === this.generation && this._token !== null && this.refreshTimer === null) {
      // Retiring an attempt does not replenish this token's deadline or fetch
      // allowance. A failed interactive login/logout resumes its original budget.
      const budget = this.refreshBudget;
      if (
        budget &&
        (performance.now() >= budget.deadline ||
          Date.now() >= budget.wallDeadline ||
          budget.attempts >= REFRESH_MAX_ATTEMPTS)
      ) {
        this.clearSession();
        return;
      }
      this.scheduleRefresh(
        budget
          ? Math.min(budget.deadline, Math.max(this.refreshDeadline, budget.nextAttemptAt))
          : this.refreshDeadline,
      );
    }
  }

  private async requestJson(
    path: string,
    body: object,
    failure: string,
    generation: number,
    signal?: AbortSignal,
    sessionMutation = false,
  ): Promise<unknown> {
    if (signal?.aborted) throw new DOMException('Authentication request retired', 'AbortError');
    let response: Response;
    try {
      response = await fetch(path, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        credentials: 'include',
        body: JSON.stringify(body),
        ...(signal ? { signal } : {}),
      });
    } catch (error) {
      if (sessionMutation) this.notifySessionMutation();
      throw error;
    }
    // Even a retired request may have installed a cookie before its response.
    if (sessionMutation && response.ok) this.notifySessionMutation();
    this.assertCurrent(generation);
    const data: unknown = await response.json().catch(() => null);
    this.assertCurrent(generation);
    if (!response.ok) {
      throw new AuthHttpRejection(
        isRecord(data) && typeof data['error'] === 'string' ? data['error'] : failure,
      );
    }
    return data;
  }

  private async establishSession(path: string, body: object, failure: string): Promise<void> {
    if (this.uncertainSession) throw new SessionOutcomeUnknownError();
    const generation = this.beginAuthentication();
    const controller = new AbortController();
    this.sessionController = controller;
    const deadline = performance.now() + SESSION_DEADLINE_MS;
    const wallDeadline = Date.now() + SESSION_DEADLINE_MS;
    const timer = setTimeout(() => controller.abort(), SESSION_DEADLINE_MS);
    let submitted = false;
    const assertLive = (): void => {
      this.assertCurrent(generation);
      if (controller.signal.aborted || performance.now() >= deadline || Date.now() >= wallDeadline)
        throw new SessionOutcomeUnknownError();
    };
    try {
      const work = async (): Promise<void> => {
        assertLive();
        submitted = true;
        const data = parseSession(
          await this.requestJson(path, body, failure, generation, controller.signal, true),
        );
        assertLive();
        if (this.sessionController === controller) this.sessionController = null;
        this.setSession(data);
      };
      await abortable(this.withSessionLock(controller.signal, work), controller.signal);
    } catch (error) {
      this.assertCurrent(generation);
      if (error instanceof AuthHttpRejection) throw error;
      if (!submitted) throw new Error('Sign-in timed out before it was sent. Please try again.');
      if (controller.signal.aborted) this.notifySessionMutation();
      // A malformed successful body, lost connection or deadline cannot prove
      // the server did not set a session cookie. Freeze new auth until reload.
      this.uncertainSession = true;
      this.clearSession();
      throw new SessionOutcomeUnknownError();
    } finally {
      clearTimeout(timer);
      if (this.sessionController === controller) this.sessionController = null;
      controller.abort();
      this.resumeRefresh(generation);
    }
  }

  private clearSession(): void {
    this.beginAuthentication();
    this._token = null;
    this._user = null;
    this.tokenExpiresAt = null;
    this.tokenExpiryDeadline = null;
    this.clearRefreshBudget();
    this.onChange?.(false, false);
  }

  private setSession(data: AuthResponse, tokenRefresh = false): void {
    const sameAccount = tokenRefresh && this._user?.id === data.user.id;
    this.stopScheduledRefresh();
    this.clearRefreshBudget();
    this._token = data.token;
    this._user = data.user;
    this.tokenExpiresAt = acceptedTokenExpiry(data.token);
    this.tokenExpiryDeadline =
      this.tokenExpiresAt === null
        ? null
        : performance.now() + Math.max(0, this.tokenExpiresAt - Date.now());
    this.scheduleRefresh();
    this.onChange?.(true, sameAccount);
  }

  private withSessionLock<T>(signal: AbortSignal, action: () => Promise<T>): Promise<T> {
    const run = (): Promise<T> => {
      if (signal.aborted)
        return Promise.reject(new DOMException('Authentication request retired', 'AbortError'));
      // Release the browser lock when a bounded caller expires, even if a
      // platform promise ignores abort; generation checks still fence results.
      return abortable(action(), signal);
    };
    return 'locks' in navigator && navigator.locks
      ? navigator.locks.request(REFRESH_LOCK_NAME, { signal }, run)
      : run();
  }

  private async requestRefresh(
    generation: number,
    scheduled?: ScheduledRefresh,
    restoreSignal?: AbortSignal,
    restoreLive?: () => void,
  ): Promise<Response> {
    const request = (): Promise<Response> => {
      this.assertCurrent(generation);
      restoreLive?.();
      if (restoreSignal?.aborted)
        throw new DOMException('Authentication request retired', 'AbortError');
      if (scheduled) {
        this.assertScheduledRefresh(scheduled);
        if (scheduled.budget.attempts >= REFRESH_MAX_ATTEMPTS) {
          throw new Error('Authentication refresh attempts exhausted');
        }
        // Count actual fetches, including exact-401 replay confirmation; waiting
        // for another tab's Web Lock does not consume or duplicate an attempt.
        scheduled.budget.attempts += 1;
        scheduled.budget.nextAttemptAt = performance.now() + refreshRetryDelay();
      }
      const signal = scheduled?.controller.signal ?? restoreSignal;
      return fetch('/api/auth/refresh', {
        method: 'POST',
        credentials: 'include',
        ...(signal ? { signal } : {}),
      });
    };

    // Refresh cookies are shared between same-origin tabs. Serializing their
    // one-time rotation prevents a routine multi-tab race from looking like
    // token theft. The direct path remains for older browsers.
    if ('locks' in navigator && navigator.locks) {
      if (scheduled) {
        return navigator.locks.request(
          REFRESH_LOCK_NAME,
          { signal: scheduled.controller.signal },
          request,
        );
      }
      if (restoreSignal)
        return navigator.locks.request(REFRESH_LOCK_NAME, { signal: restoreSignal }, request);
      return navigator.locks.request(REFRESH_LOCK_NAME, request);
    }
    return request();
  }

  private async requestRefreshWithReplayConfirmation(
    generation: number,
    scheduled?: ScheduledRefresh,
    restoreSignal?: AbortSignal,
    restoreLive?: () => void,
  ): Promise<Response> {
    const response = await this.requestRefresh(generation, scheduled, restoreSignal, restoreLive);
    this.assertCurrent(generation);
    restoreLive?.();
    if (scheduled) this.assertScheduledRefresh(scheduled);
    const rejected = await isRejectedRefreshToken(response);
    this.assertCurrent(generation);
    restoreLive?.();
    if (scheduled) this.assertScheduledRefresh(scheduled);
    if (!rejected) return response;

    await delay(REFRESH_REPLAY_CONFIRM_DELAY_MS, scheduled?.controller.signal ?? restoreSignal);
    return this.requestRefresh(generation, scheduled, restoreSignal, restoreLive);
  }

  private scheduleRefresh(deadline = performance.now() + 12 * 60 * 1000): void {
    if (this.refreshTimer !== null) clearTimeout(this.refreshTimer);
    this.refreshDeadline = Math.min(deadline, this.tokenExpiryDeadline ?? deadline);
    const generation = this.generation;
    // JWT lifetime is 15 minutes — refresh at 12 minutes
    this.refreshTimer = setTimeout(
      () => {
        if (generation !== this.generation) return;
        this.refreshTimer = null;
        this.refresh(generation).catch(() => {
          if (generation === this.generation) this.clearSession();
        });
      },
      Math.max(0, this.refreshDeadline - performance.now()),
    );
  }

  private async refresh(generation: number): Promise<void> {
    this.assertCurrent(generation);
    if (this.scheduledRefresh) return;
    const now = performance.now();
    const budget = this.refreshBudget ?? {
      deadline: Math.min(now + REFRESH_RETRY_WINDOW_MS, this.tokenExpiryDeadline ?? Infinity),
      wallDeadline: Math.min(Date.now() + REFRESH_RETRY_WINDOW_MS, this.tokenExpiresAt ?? Infinity),
      attempts: 0,
      nextAttemptAt: now,
      expiryTimer: null,
    };
    this.refreshBudget = budget;
    const scheduled: ScheduledRefresh = {
      generation,
      budget,
      controller: new AbortController(),
    };
    this.scheduledRefresh = scheduled;
    if (this.refreshExpired(scheduled)) {
      this.clearSession();
      return;
    }
    // One watchdog covers lock admission, fetch, response parsing and all retry
    // delays. Neither a slow response nor a retry restarts the retention window.
    if (budget.expiryTimer === null) {
      const token = this._token;
      budget.expiryTimer = setTimeout(
        () => {
          // The accepted token owns retention even if an interactive operation
          // retires its refresh task. A new accepted session replaces this budget.
          if (this.refreshBudget === budget && this._token === token) this.clearSession();
        },
        Math.max(0, budget.deadline - now),
      );
    }
    await this.runScheduledRefresh(scheduled);
  }

  private async runScheduledRefresh(scheduled: ScheduledRefresh): Promise<void> {
    let response: Response;
    try {
      response = await this.requestRefreshWithReplayConfirmation(scheduled.generation, scheduled);
    } catch {
      this.retryRefresh(scheduled);
      return;
    }
    if (!this.ownsScheduledRefresh(scheduled)) return;
    if (this.refreshExpired(scheduled)) {
      this.clearSession();
      return;
    }
    if (response.status >= 500 && response.status <= 599) {
      this.retryRefresh(scheduled);
      return;
    }
    if (!response.ok) {
      this.clearSession();
      return;
    }
    let data: AuthResponse;
    try {
      data = await readSession(response);
    } catch {
      // Successful-but-malformed responses are not temporary server errors and
      // cannot replace the accepted identity with partially decoded new claims.
      if (this.ownsScheduledRefresh(scheduled)) this.clearSession();
      return;
    }
    if (!this.ownsScheduledRefresh(scheduled)) return;
    if (this.refreshExpired(scheduled)) {
      this.clearSession();
      return;
    }
    // Refresh updates the signaling JWT without treating it as a new login.
    this.setSession(data, true);
  }

  private retryRefresh(scheduled: ScheduledRefresh): void {
    if (!this.ownsScheduledRefresh(scheduled)) return;
    if (
      this.tokenExpiryDeadline === null ||
      this.refreshExpired(scheduled) ||
      scheduled.budget.attempts >= REFRESH_MAX_ATTEMPTS
    ) {
      this.clearSession();
      return;
    }
    const now = performance.now();
    const retryAt = Math.min(scheduled.budget.deadline, now + refreshRetryDelay());
    scheduled.budget.nextAttemptAt = retryAt;
    this.refreshDeadline = retryAt;
    if (this.refreshTimer !== null) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(
      () => {
        if (!this.ownsScheduledRefresh(scheduled)) return;
        this.refreshTimer = null;
        if (this.refreshExpired(scheduled)) {
          this.clearSession();
          return;
        }
        this.runScheduledRefresh(scheduled).catch(() => {
          if (this.ownsScheduledRefresh(scheduled)) this.clearSession();
        });
      },
      Math.max(0, retryAt - now),
    );
  }

  private ownsScheduledRefresh(scheduled: ScheduledRefresh): boolean {
    return this.scheduledRefresh === scheduled && this.generation === scheduled.generation;
  }

  private refreshExpired(scheduled: ScheduledRefresh): boolean {
    return (
      performance.now() >= scheduled.budget.deadline ||
      Date.now() >= scheduled.budget.wallDeadline ||
      (this.tokenExpiryDeadline !== null && performance.now() >= this.tokenExpiryDeadline) ||
      (this.tokenExpiresAt !== null && Date.now() >= this.tokenExpiresAt)
    );
  }

  private assertScheduledRefresh(scheduled: ScheduledRefresh): void {
    if (!this.ownsScheduledRefresh(scheduled) || this.refreshExpired(scheduled)) {
      throw new Error('Authentication request superseded');
    }
  }

  private stopScheduledRefresh(): void {
    const scheduled = this.scheduledRefresh;
    this.scheduledRefresh = null;
    if (!scheduled) return;
    scheduled.controller.abort();
  }

  private clearRefreshBudget(): void {
    const budget = this.refreshBudget;
    if (budget && budget.expiryTimer !== null) clearTimeout(budget.expiryTimer);
    this.refreshBudget = null;
  }
}

function refreshRetryDelay(): number {
  return REFRESH_RETRY_DELAY_MS + Math.floor(Math.random() * REFRESH_RETRY_JITTER_MS);
}

/** Decode only expiry from an already accepted server response to shorten local
 * retention. This does not verify a signature or authorize any identity; the
 * server still validates every token. Unknown expiry disables transient retry. */
function acceptedTokenExpiry(token: string): number | null {
  try {
    if (token.length > 4096) return null;
    const payload = token.split('.')[1];
    if (!payload) return null;
    const decoded: unknown = JSON.parse(atob(payload.replace(/-/g, '+').replace(/_/g, '/')));
    if (!isRecord(decoded)) return null;
    const expiry = decoded['exp'];
    return typeof expiry === 'number' &&
      Number.isSafeInteger(expiry) &&
      expiry > 0 &&
      Number.isSafeInteger(expiry * 1000)
      ? expiry * 1000
      : null;
  } catch {
    return null;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function record(value: unknown): Record<string, unknown> {
  if (!isRecord(value)) throw new Error('Authentication response was incomplete');
  return value;
}

function string(value: unknown): string {
  if (typeof value !== 'string' || !value)
    throw new Error('Authentication response was incomplete');
  return value;
}

function parseSession(value: unknown): AuthResponse {
  const data = record(value);
  const user = record(data['user']);
  return {
    token: string(data['token']),
    user: {
      id: string(user['id']),
      email: string(user['email']),
      display_name: string(user['display_name']),
    },
  };
}

async function readSession(response: Response): Promise<AuthResponse> {
  const data: unknown = await response.json();
  return parseSession(data);
}

async function isRejectedRefreshToken(response: Response): Promise<boolean> {
  if (response.status !== 401) return false;
  const body: unknown = await response
    .clone()
    .json()
    .catch(() => null);
  return (
    typeof body === 'object' && body !== null && 'error' in body && body.error === 'Invalid token'
  );
}

function delay(milliseconds: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new Error('Authentication request superseded'));
      return;
    }
    const abort = (): void => {
      clearTimeout(timer);
      reject(new Error('Authentication request superseded'));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', abort);
      resolve();
    }, milliseconds);
    signal?.addEventListener('abort', abort, { once: true });
  });
}

// --- WebAuthn serialization helpers ---
// WebAuthn APIs use ArrayBuffer but JSON needs base64url

function base64urlEncode(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let str = '';
  for (const b of bytes) str += String.fromCharCode(b);
  return btoa(str).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function base64urlDecode(str: string): ArrayBuffer {
  const padded = str.replace(/-/g, '+').replace(/_/g, '/');
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

function descriptors(value: unknown): PublicKeyCredentialDescriptor[] {
  if (!Array.isArray(value)) throw new Error('Passkey credentials were incomplete');
  return value.map((entry: unknown) => {
    const descriptor = record(entry);
    if (descriptor['type'] !== 'public-key') throw new Error('Unsupported passkey credential type');
    return { ...descriptor, type: 'public-key', id: base64urlDecode(string(descriptor['id'])) };
  });
}

export function deserializeCreationOptions(
  options: Record<string, unknown>,
): CredentialCreationOptions {
  const pk = record(options['publicKey']);
  const rp = record(pk['rp']);
  const user = record(pk['user']);
  const params = pk['pubKeyCredParams'];
  if (!Array.isArray(params)) throw new Error('Passkey algorithms were incomplete');
  const pubKeyCredParams: PublicKeyCredentialParameters[] = params.map((entry: unknown) => {
    const param = record(entry);
    const alg = param['alg'];
    if (param['type'] !== 'public-key' || typeof alg !== 'number' || !Number.isInteger(alg)) {
      throw new Error('Unsupported passkey algorithm');
    }
    return { type: 'public-key', alg };
  });
  // Preserve optional WebAuthn settings for the browser to validate; convert
  // the required fields and binary descriptors without mutating server JSON.
  return {
    publicKey: {
      ...pk,
      challenge: base64urlDecode(string(pk['challenge'])),
      rp: { ...rp, name: string(rp['name']) },
      user: {
        ...user,
        id: base64urlDecode(string(user['id'])),
        name: string(user['name']),
        displayName: string(user['displayName']),
      },
      pubKeyCredParams,
      ...(pk['excludeCredentials'] === undefined
        ? {}
        : { excludeCredentials: descriptors(pk['excludeCredentials']) }),
    },
  };
}

export function deserializeRequestOptions(
  options: Record<string, unknown>,
): CredentialRequestOptions {
  const pk = record(options['publicKey']);
  const mediation = options['mediation'];
  if (
    mediation !== undefined &&
    mediation !== 'required' &&
    mediation !== 'optional' &&
    mediation !== 'conditional' &&
    mediation !== 'silent'
  )
    throw new Error('Unsupported passkey mediation');
  return {
    ...(mediation === undefined ? {} : { mediation }),
    publicKey: {
      ...pk,
      challenge: base64urlDecode(string(pk['challenge'])),
      ...(pk['allowCredentials'] === undefined
        ? {}
        : { allowCredentials: descriptors(pk['allowCredentials']) }),
    },
  };
}

export function serializeCredential(cred: Credential): {
  id: string;
  rawId: string;
  type: string;
  response: Record<string, string>;
  clientExtensionResults?: { credProps?: { rk: boolean }; appid?: boolean };
} {
  if (!(cred instanceof PublicKeyCredential)) throw new Error('Expected a public-key credential');
  const response = cred.response;
  const encoded: Record<string, string> = {
    clientDataJSON: base64urlEncode(response.clientDataJSON),
  };
  if (response instanceof AuthenticatorAttestationResponse) {
    encoded['attestationObject'] = base64urlEncode(response.attestationObject);
  } else if (response instanceof AuthenticatorAssertionResponse) {
    encoded['authenticatorData'] = base64urlEncode(response.authenticatorData);
    encoded['signature'] = base64urlEncode(response.signature);
    if (response.userHandle) encoded['userHandle'] = base64urlEncode(response.userHandle);
  } else {
    throw new Error('Unsupported passkey response');
  }
  // Forward only the properties understood by this server. Extension results
  // can contain unrelated binary outputs; they are not evidence of residency.
  const extensions = cred.getClientExtensionResults();
  const clientExtensionResults: { credProps?: { rk: boolean }; appid?: boolean } = {};
  const rk = extensions.credProps?.rk;
  if (typeof rk === 'boolean') clientExtensionResults.credProps = { rk };
  if (typeof extensions.appid === 'boolean') clientExtensionResults.appid = extensions.appid;
  return {
    id: cred.id,
    rawId: base64urlEncode(cred.rawId),
    type: cred.type,
    response: encoded,
    ...(Object.keys(clientExtensionResults).length > 0 ? { clientExtensionResults } : {}),
  };
}

/** Consume late rejections while immediately retiring the caller on abort. */
function abortable<T>(work: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const abort = (): void =>
      reject(new DOMException('Authentication request retired', 'AbortError'));
    if (signal.aborted) abort();
    else signal.addEventListener('abort', abort, { once: true });
    work.then(
      (value) => {
        signal.removeEventListener('abort', abort);
        resolve(value);
      },
      (error: unknown) => {
        signal.removeEventListener('abort', abort);
        reject(error instanceof Error ? error : new Error('Authentication restoration failed'));
      },
    );
  });
}
