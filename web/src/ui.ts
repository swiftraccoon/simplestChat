/** Small, text-safe controls shared by the community screens. */
export function el<K extends keyof HTMLElementTagNameMap>(tag: K, text?: string, className?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}

export function button(text: string, action: () => unknown, className = 'btn-secondary'): HTMLButtonElement {
  const node = el('button', text, className);
  node.type = 'button';
  node.addEventListener('click', () => { action(); });
  return node;
}

export function field(label: string, input: HTMLElement): HTMLLabelElement {
  const wrapper = el('label', undefined, 'community-field');
  wrapper.append(el('span', label), input);
  return wrapper;
}

export function input(value = '', type = 'text', maxLength = 128): HTMLInputElement {
  const node = el('input');
  node.type = type;
  node.value = value;
  node.maxLength = maxLength;
  return node;
}

export function modal(title: string): { dialog: HTMLDialogElement; body: HTMLDivElement; error: HTMLParagraphElement; close: () => void } {
  const dialog = el('dialog', undefined, 'community-dialog');
  const heading = el('h2', title);
  heading.id = `dialog-${crypto.randomUUID()}`;
  dialog.setAttribute('aria-labelledby', heading.id);
  const header = el('div', undefined, 'community-dialog-header');
  const close = (): void => { dialog.close(); };
  const closeButton = button('Close', close);
  header.append(heading, closeButton);
  const body = el('div', undefined, 'community-dialog-body');
  const error = el('p', '', 'auth-error');
  error.hidden = true;
  error.setAttribute('role', 'alert');
  dialog.append(header, error, body);
  dialog.addEventListener('close', () => dialog.remove(), { once: true });
  dialog.addEventListener('click', event => {
    if (event.target !== dialog) return;
    const rect = dialog.getBoundingClientRect();
    if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) close();
  });
  document.body.append(dialog);
  dialog.showModal();
  return { dialog, body, error, close };
}

export async function busy(buttonNode: HTMLButtonElement, error: HTMLElement, work: () => Promise<void>): Promise<void> {
  if (buttonNode.disabled) return;
  buttonNode.disabled = true;
  error.hidden = true;
  try {
    await work();
  } catch (failure) {
    error.textContent = failure instanceof Error ? failure.message : 'The action could not be completed';
    error.hidden = false;
  } finally {
    buttonNode.disabled = false;
  }
}

export async function api<T>(path: string, token: string | null, method = 'GET', data?: unknown): Promise<T> {
  const response = await fetch(path, {
    method,
    credentials: 'same-origin',
    headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}), ...(data === undefined ? {} : { 'Content-Type': 'application/json' }) },
    ...(data === undefined ? {} : { body: JSON.stringify(data) }),
  });
  if (!response.ok) {
    const raw = await response.text();
    let message = raw;
    try { message = (JSON.parse(raw) as { error?: string }).error ?? raw; } catch { /* Text API error. */ }
    throw new Error(message.slice(0, 400) || `Request failed (${response.status})`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

/** Decode and re-encode raster uploads; never inject file markup or remote URLs. */
export async function rasterUpload(file: File, width = 192, height = 192): Promise<string> {
  if (!['image/png', 'image/jpeg', 'image/webp'].includes(file.type) || file.size > 8 * 1024 * 1024) {
    throw new Error('Choose a PNG, JPEG or WebP image smaller than 8 MB');
  }
  const url = URL.createObjectURL(file);
  try {
    const image = new Image();
    image.src = url;
    await image.decode();
    if (!image.naturalWidth || !image.naturalHeight) throw new Error('Image could not be decoded');
    const canvas = el('canvas');
    canvas.width = width;
    canvas.height = height;
    const context = canvas.getContext('2d');
    if (!context) throw new Error('Image processing is unavailable');
    const scale = Math.max(width / image.naturalWidth, height / image.naturalHeight);
    const sourceWidth = width / scale;
    const sourceHeight = height / scale;
    context.drawImage(image, (image.naturalWidth - sourceWidth) / 2, (image.naturalHeight - sourceHeight) / 2,
      sourceWidth, sourceHeight, 0, 0, width, height);
    const result = canvas.toDataURL('image/jpeg', 0.8);
    if (result.length > 170_000) throw new Error('Image is too detailed; please choose a smaller image');
    return result;
  } finally {
    URL.revokeObjectURL(url);
  }
}

export function safeRasterUrl(value: unknown): value is string {
  return typeof value === 'string' && value.length <= 180_000 && /^data:image\/(?:png|jpeg|webp);base64,[A-Za-z0-9+/=]+$/.test(value);
}
