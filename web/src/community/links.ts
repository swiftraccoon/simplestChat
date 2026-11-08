export function roomLink(id: string): string {
  const url = new URL(window.location.href);
  url.hash = id;
  url.search = '';
  return url.href;
}

/** Keep invitation secrets in the fragment, which browsers never send in an HTTP request. */
export function inviteLink(code: string, kind: 'room' | 'registration' = 'room'): string {
  const url = new URL(window.location.href);
  url.search = '';
  url.hash = `${kind === 'registration' ? 'register-invite' : 'invite'}=${encodeURIComponent(code)}`;
  return url.href;
}
