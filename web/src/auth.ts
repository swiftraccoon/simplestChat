import type { AuthResponse, UserInfo } from './protocol';

type AuthChangeHandler = (loggedIn: boolean) => void;

export class AuthManager {
  private _token: string | null = null;
  private _user: UserInfo | null = null;
  private refreshTimer: ReturnType<typeof setTimeout> | null = null;
  private onChange: AuthChangeHandler | null = null;

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

  setOnChange(handler: AuthChangeHandler): void {
    this.onChange = handler;
  }

  /** Try to restore session from refresh token cookie on page load */
  async tryRestore(): Promise<boolean> {
    try {
      const resp = await fetch('/api/auth/refresh', {
        method: 'POST',
        credentials: 'include',
      });
      if (!resp.ok) return false;
      const data: AuthResponse = await resp.json();
      this.setSession(data);
      return true;
    } catch {
      return false;
    }
  }

  async register(email: string, displayName: string, password: string): Promise<void> {
    const resp = await fetch('/api/auth/register', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ email, password, display_name: displayName }),
    });
    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: 'Registration failed' }));
      throw new Error(err.error ?? 'Registration failed');
    }
    const data: AuthResponse = await resp.json();
    this.setSession(data);
  }

  async login(email: string, password: string): Promise<void> {
    const resp = await fetch('/api/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ email, password }),
    });
    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: 'Login failed' }));
      throw new Error(err.error ?? 'Login failed');
    }
    const data: AuthResponse = await resp.json();
    this.setSession(data);
  }

  async passkeyRegisterStart(email: string, displayName: string): Promise<CredentialCreationOptions> {
    const resp = await fetch('/api/auth/passkey/register/start', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email, password: 'unused', display_name: displayName }),
    });
    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: 'Passkey registration failed' }));
      throw new Error(err.error ?? 'Passkey registration failed');
    }
    const options = await resp.json();
    return deserializeCreationOptions(options);
  }

  async passkeyRegisterFinish(email: string, credential: Credential): Promise<void> {
    const resp = await fetch('/api/auth/passkey/register/finish', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ email, credential: serializeCredential(credential) }),
    });
    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: 'Passkey registration failed' }));
      throw new Error(err.error ?? 'Passkey registration failed');
    }
    const data: AuthResponse = await resp.json();
    this.setSession(data);
  }

  async passkeyLoginStart(email: string): Promise<CredentialRequestOptions> {
    const resp = await fetch('/api/auth/passkey/login/start', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email }),
    });
    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: 'Passkey login failed' }));
      throw new Error(err.error ?? 'Passkey login failed');
    }
    const options = await resp.json();
    return deserializeRequestOptions(options);
  }

  async passkeyLoginFinish(email: string, credential: Credential): Promise<void> {
    const resp = await fetch('/api/auth/passkey/login/finish', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      credentials: 'include',
      body: JSON.stringify({ email, credential: serializeCredential(credential) }),
    });
    if (!resp.ok) {
      const err = await resp.json().catch(() => ({ error: 'Passkey login failed' }));
      throw new Error(err.error ?? 'Passkey login failed');
    }
    const data: AuthResponse = await resp.json();
    this.setSession(data);
  }

  logout(): void {
    this._token = null;
    this._user = null;
    if (this.refreshTimer) {
      clearTimeout(this.refreshTimer);
      this.refreshTimer = null;
    }
    this.onChange?.(false);
  }

  private setSession(data: AuthResponse): void {
    this._token = data.token;
    this._user = data.user;
    this.scheduleRefresh();
    this.onChange?.(true);
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    // JWT lifetime is 15 minutes — refresh at 12 minutes
    this.refreshTimer = setTimeout(async () => {
      try {
        const resp = await fetch('/api/auth/refresh', {
          method: 'POST',
          credentials: 'include',
        });
        if (resp.ok) {
          const data: AuthResponse = await resp.json();
          this._token = data.token;
          this._user = data.user;
          this.scheduleRefresh();
        } else {
          this.logout();
        }
      } catch {
        this.logout();
      }
    }, 12 * 60 * 1000);
  }
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

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function deserializeCreationOptions(options: any): CredentialCreationOptions {
  const pk = options.publicKey;
  pk.challenge = base64urlDecode(pk.challenge);
  pk.user.id = base64urlDecode(pk.user.id);
  if (pk.excludeCredentials) {
    for (const c of pk.excludeCredentials) {
      c.id = base64urlDecode(c.id);
    }
  }
  return { publicKey: pk };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function deserializeRequestOptions(options: any): CredentialRequestOptions {
  const pk = options.publicKey;
  pk.challenge = base64urlDecode(pk.challenge);
  if (pk.allowCredentials) {
    for (const c of pk.allowCredentials) {
      c.id = base64urlDecode(c.id);
    }
  }
  return { publicKey: pk };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function serializeCredential(cred: Credential): any {
  const pkCred = cred as PublicKeyCredential;
  const response = pkCred.response as AuthenticatorAttestationResponse | AuthenticatorAssertionResponse;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const result: any = {
    id: pkCred.id,
    rawId: base64urlEncode(pkCred.rawId),
    type: pkCred.type,
    response: {} as Record<string, string>,
  };
  if ('attestationObject' in response) {
    result.response.attestationObject = base64urlEncode((response as AuthenticatorAttestationResponse).attestationObject);
    result.response.clientDataJSON = base64urlEncode(response.clientDataJSON);
  } else {
    result.response.authenticatorData = base64urlEncode((response as AuthenticatorAssertionResponse).authenticatorData);
    result.response.clientDataJSON = base64urlEncode(response.clientDataJSON);
    result.response.signature = base64urlEncode((response as AuthenticatorAssertionResponse).signature);
    const userHandle = (response as AuthenticatorAssertionResponse).userHandle;
    if (userHandle) result.response.userHandle = base64urlEncode(userHandle);
  }
  return result;
}
