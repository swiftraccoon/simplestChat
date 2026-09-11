import type { AuthResponse, UserInfo } from './protocol';

type AuthChangeHandler = (loggedIn: boolean, tokenRefresh: boolean) => void;

const REFRESH_LOCK_NAME = 'simplestchat-refresh-v1';
// The server rejects, but does not revoke, the exact predecessor for two
// seconds so simultaneous tabs do not destroy the winning refresh. Retrying
// after that grace either uses the shared successor cookie or revokes a
// successor created on another device with a stolen token.
const REFRESH_REPLAY_CONFIRM_DELAY_MS = 2_250;

export class AuthManager {
  private _token: string | null = null;
  private _user: UserInfo | null = null;
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
  private refreshDeadline = 0;
  private onChange: AuthChangeHandler | null = null;
  private registrationCeremonyId: string | null = null;
  private authenticationCeremonyId: string | null = null;
  private generation = 0;

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
  }

  setOnChange(handler: AuthChangeHandler): void {
    this.onChange = handler;
  }

  /** Try to restore session from refresh token cookie on page load */
  async tryRestore(): Promise<boolean> {
    const generation = this.beginAuthentication();
    try {
      const resp = await this.requestRefreshWithReplayConfirmation(generation);
      if (!resp.ok) return false;
      const data = await readSession(resp);
      this.assertCurrent(generation);
      this.setSession(data);
      return true;
    } catch {
      return false;
    } finally {
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
  ): Promise<CredentialCreationOptions> {
    const generation = this.beginAuthentication();
    try {
      const options = record(
        await this.requestJson(
          '/api/auth/passkey/register/start',
          { email, display_name: displayName },
          'Passkey registration failed',
          generation,
        ),
      );
      this.assertCurrent(generation);
      const ceremonyId = string(options['ceremony_id']);
      const result = deserializeCreationOptions(options);
      this.registrationCeremonyId = ceremonyId;
      return result;
    } finally {
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

  async passkeyLoginStart(email: string): Promise<CredentialRequestOptions> {
    const generation = this.beginAuthentication();
    try {
      const options = record(
        await this.requestJson(
          '/api/auth/passkey/login/start',
          { email },
          'Passkey login failed',
          generation,
        ),
      );
      this.assertCurrent(generation);
      const ceremonyId = string(options['ceremony_id']);
      const result = deserializeRequestOptions(options);
      this.authenticationCeremonyId = ceremonyId;
      return result;
    } finally {
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
    try {
      const response = await fetch('/api/auth/logout', { method: 'POST', credentials: 'include' });
      this.assertCurrent(generation);
      if (!response.ok) {
        throw new Error('Sign out could not revoke the session; please try again');
      }
      this.clearSession();
    } finally {
      this.resumeRefresh(generation);
    }
  }

  // Only the latest local authentication intent may adopt or retire identity.
  // This does not cancel server-side effects of requests already sent.
  private beginAuthentication(): number {
    this.generation += 1;
    if (this.refreshTimer !== null) clearTimeout(this.refreshTimer);
    this.refreshTimer = null;
    this.registrationCeremonyId = null;
    this.authenticationCeremonyId = null;
    return this.generation;
  }

  private assertCurrent(generation: number): void {
    if (generation !== this.generation) throw new Error('Authentication request superseded');
  }

  private resumeRefresh(generation: number): void {
    if (generation === this.generation && this._token !== null && this.refreshTimer === null) {
      this.scheduleRefresh(this.refreshDeadline);
    }
  }

  private async requestJson(
    path: string,
    body: object,
    failure: string,
    generation: number,
  ): Promise<unknown> {
    const response = await fetch(path, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify(body),
    });
    this.assertCurrent(generation);
    const data: unknown = await response.json().catch(() => null);
    this.assertCurrent(generation);
    if (!response.ok) {
      throw new Error(
        isRecord(data) && typeof data['error'] === 'string' ? data['error'] : failure,
      );
    }
    return data;
  }

  private async establishSession(path: string, body: object, failure: string): Promise<void> {
    const generation = this.beginAuthentication();
    try {
      const data = parseSession(await this.requestJson(path, body, failure, generation));
      this.assertCurrent(generation);
      this.setSession(data);
    } finally {
      this.resumeRefresh(generation);
    }
  }

  private clearSession(): void {
    this.beginAuthentication();
    this._token = null;
    this._user = null;
    this.onChange?.(false, false);
  }

  private setSession(data: AuthResponse, tokenRefresh = false): void {
    this._token = data.token;
    this._user = data.user;
    this.scheduleRefresh();
    this.onChange?.(true, tokenRefresh);
  }

  private async requestRefresh(generation: number): Promise<Response> {
    const request = (): Promise<Response> => {
      this.assertCurrent(generation);
      return fetch('/api/auth/refresh', { method: 'POST', credentials: 'include' });
    };

    // Refresh cookies are shared between same-origin tabs. Serializing their
    // one-time rotation prevents a routine multi-tab race from looking like
    // token theft. The direct path remains for older browsers.
    if ('locks' in navigator && navigator.locks) {
      return navigator.locks.request(REFRESH_LOCK_NAME, request);
    }
    return request();
  }

  private async requestRefreshWithReplayConfirmation(generation: number): Promise<Response> {
    const response = await this.requestRefresh(generation);
    this.assertCurrent(generation);
    const rejected = await isRejectedRefreshToken(response);
    this.assertCurrent(generation);
    if (!rejected) return response;

    await delay(REFRESH_REPLAY_CONFIRM_DELAY_MS);
    return this.requestRefresh(generation);
  }

  private scheduleRefresh(deadline = Date.now() + 12 * 60 * 1000): void {
    if (this.refreshTimer !== null) clearTimeout(this.refreshTimer);
    this.refreshDeadline = deadline;
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
      Math.max(0, deadline - Date.now()),
    );
  }

  private async refresh(generation: number): Promise<void> {
    const response = await this.requestRefreshWithReplayConfirmation(generation);
    this.assertCurrent(generation);
    if (!response.ok) {
      this.clearSession();
      return;
    }
    const data = await readSession(response);
    this.assertCurrent(generation);
    // Refresh updates the signaling JWT without treating it as a new login.
    this.setSession(data, true);
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

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
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

function deserializeCreationOptions(options: Record<string, unknown>): CredentialCreationOptions {
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

function deserializeRequestOptions(options: Record<string, unknown>): CredentialRequestOptions {
  const pk = record(options['publicKey']);
  return {
    publicKey: {
      ...pk,
      challenge: base64urlDecode(string(pk['challenge'])),
      ...(pk['allowCredentials'] === undefined
        ? {}
        : { allowCredentials: descriptors(pk['allowCredentials']) }),
    },
  };
}

function serializeCredential(cred: Credential): {
  id: string;
  rawId: string;
  type: string;
  response: Record<string, string>;
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
  return { id: cred.id, rawId: base64urlEncode(cred.rawId), type: cred.type, response: encoded };
}
