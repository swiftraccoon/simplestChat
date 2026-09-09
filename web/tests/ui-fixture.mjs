import { loadTypeScript } from './source-loader.mjs';

export function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

export async function flush() {
  for (let index = 0; index < 12; index++) await Promise.resolve();
}

/** Minimal text-only DOM: any accidental HTML rendering fails the test. */
export function createDOM() {
  const created = [];
  let body;
  class Node {
    children = [];
    parentNode = null;
    attributes = new Map();
    listeners = new Map();
    hidden = false;
    disabled = false;
    open = false;
    value = '';
    _text = '';
    constructor(tagName) { this.tagName = tagName.toUpperCase(); created.push(this); }
    get isConnected() { return this === body || Boolean(this.parentNode?.isConnected); }
    get textContent() { return this._text + this.children.map(child => child.textContent).join(''); }
    set textContent(value) { this.replaceChildren(); this._text = String(value); }
    set innerHTML(_value) { throw new Error('Unexpected HTML rendering'); }
    append(...nodes) { for (const node of nodes) { node.remove(); node.parentNode = this; this.children.push(node); } }
    appendChild(node) { this.append(node); return node; }
    replaceChildren(...nodes) { for (const node of this.children) node.parentNode = null; this.children = []; this._text = ''; this.append(...nodes); }
    remove() { if (this.parentNode) { this.parentNode.children = this.parentNode.children.filter(node => node !== this); this.parentNode = null; } }
    setAttribute(key, value) { this.attributes.set(key, String(value)); }
    getAttribute(key) { return this.attributes.get(key) ?? null; }
    removeAttribute(key) { this.attributes.delete(key); if (key === 'src') delete this.src; }
    addEventListener(type, callback, options = {}) {
      const entries = this.listeners.get(type) ?? [];
      entries.push({ callback, once: options.once }); this.listeners.set(type, entries);
    }
    emit(type, event = {}) {
      for (const entry of [...(this.listeners.get(type) ?? [])]) {
        if (entry.once) this.listeners.set(type, this.listeners.get(type).filter(item => item !== entry));
        entry.callback({ target: this, ...event });
      }
    }
    click() { if (!this.disabled) this.emit('click'); }
    showModal() { this.open = true; }
    close() { if (this.open) { this.open = false; this.emit('close'); } }
    getBoundingClientRect() { return { left: 10, top: 10, right: 200, bottom: 200 }; }
    querySelectorAll(selector) {
      const selectors = selector.split(',').map(value => value.trim().toUpperCase());
      return this.children.flatMap(child => [
        ...(selectors.some(value => value === child.tagName || value === `#${child.id?.toUpperCase()}` ||
          (value.startsWith('.') && (child.className ?? '').toUpperCase().split(/\s+/).includes(value.slice(1))) ||
          (value === 'DIALOG[OPEN]' && child.tagName === 'DIALOG' && child.open)) ? [child] : []),
        ...child.querySelectorAll(selector),
      ]);
    }
    querySelector(selector) { return this.querySelectorAll(selector)[0] ?? null; }
  }
  body = new Node('body');
  const header = new Node('div'); header.id = 'community-actions';
  const loginActions = new Node('div');
  body.append(header, loginActions);
  const document = {
    body,
    createElement: tag => new Node(tag),
    getElementById: id => created.find(node => node.id === id && node.isConnected) ?? null,
    querySelector: selector => selector === '#login-modal .auth-alt-actions' ? loginActions : body.querySelector(selector),
    querySelectorAll: selector => body.querySelectorAll(selector),
  };
  return { document, created, Node, header, loginActions };
}

export async function uiFixture(extraGlobals = {}) {
  const dom = createDOM();
  const state = { requests: [], revoked: [], objectUrls: [], draws: [], encoded: [], imageWidth: 400, imageHeight: 200,
    decode: async () => {}, dataUrl: 'data:image/jpeg;base64,YQ==', contextAvailable: true };
  const createElement = dom.document.createElement;
  dom.document.createElement = tag => {
    const node = createElement(tag);
    if (tag === 'canvas') {
      node.getContext = () => state.contextAvailable ? { drawImage: (...args) => state.draws.push(args) } : null;
      node.toDataURL = (...args) => { state.encoded.push(args); return state.dataUrl; };
    }
    return node;
  };
  class Image {
    get naturalWidth() { return state.imageWidth; }
    get naturalHeight() { return state.imageHeight; }
    decode() { return state.decode(); }
  }
  const globals = {
    document: dom.document,
    crypto: { randomUUID: () => `test-${dom.created.length}` },
    Image,
    URL: {
      createObjectURL(file) { state.objectUrls.push(file); return 'blob:test-upload'; },
      revokeObjectURL(url) { state.revoked.push(url); },
    },
    fetch: async (...args) => { state.requests.push(args); return state.response; },
    ...extraGlobals,
  };
  const ui = await loadTypeScript('src/ui.ts', { globals });
  return { ...dom, state, ui, globals };
}
