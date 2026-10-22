import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { queryElements } from '../src/dom.ts';

const html = await readFile(new URL('../index.html', import.meta.url), 'utf8');

/** Model element identity and browser constructors using the real static markup. */
function fixture() {
  class HTMLElement {
    constructor(localName, attributes) {
      this.localName = localName;
      this.id = attributes.id ?? '';
      this.className = attributes.class ?? '';
      this.dataset = { tab: attributes['data-tab'] };
      this.parentElement = null;
    }
  }
  const types = {
    a: class HTMLAnchorElement extends HTMLElement {},
    button: class HTMLButtonElement extends HTMLElement {},
    div: class HTMLDivElement extends HTMLElement {},
    span: class HTMLSpanElement extends HTMLElement {},
    input: class HTMLInputElement extends HTMLElement {},
    textarea: class HTMLTextAreaElement extends HTMLElement {},
    ul: class HTMLUListElement extends HTMLElement {},
    h2: class HTMLHeadingElement extends HTMLElement {},
    p: class HTMLParagraphElement extends HTMLElement {},
    dialog: class HTMLDialogElement extends HTMLElement {},
  };
  const nodes = [];
  const ancestors = [];
  const voidElements = new Set([
    'area',
    'base',
    'br',
    'col',
    'embed',
    'hr',
    'img',
    'input',
    'link',
    'meta',
  ]);
  for (const match of html.matchAll(/<(\/?)([a-z][\w-]*)\b([^>]*)>/gi)) {
    const [, close, localName, source] = match;
    if (close) {
      while (ancestors.length > 0 && ancestors.pop().localName !== localName) {
        // Skip intervening open elements when the HTML omits a closing tag.
      }
      continue;
    }
    const attributes = Object.fromEntries(
      [...source.matchAll(/([\w-]+)(?:="([^"]*)")?/g)].map((attribute) => [
        attribute[1],
        attribute[2] ?? '',
      ]),
    );
    const constructor = types[localName] ?? HTMLElement;
    const element = new constructor(localName, attributes);
    element.parentElement = ancestors.at(-1) ?? null;
    nodes.push(element);
    if (!voidElements.has(localName)) ancestors.push(element);
  }

  function matches(element, selector) {
    if (selector.startsWith('#')) return element.id === selector.slice(1);
    if (selector.startsWith('.')) return element.className.split(/\s+/).includes(selector.slice(1));
    return element.localName === selector;
  }
  function select(selector) {
    const parts = selector.split(' ');
    return nodes.filter((element) => {
      if (!matches(element, parts.at(-1))) return false;
      let ancestor = element.parentElement;
      for (let index = parts.length - 2; index >= 0; index--) {
        while (ancestor && !matches(ancestor, parts[index])) ancestor = ancestor.parentElement;
        if (!ancestor) return false;
        ancestor = ancestor.parentElement;
      }
      return true;
    });
  }
  const queries = new Map();
  const selected = new Map();
  const record = (selector, elements) => {
    queries.set(selector, (queries.get(selector) ?? 0) + 1);
    for (const element of elements) selected.set(element, (selected.get(element) ?? 0) + 1);
    return elements;
  };
  const document = {
    defaultView: {
      HTMLElement,
      ...Object.fromEntries(
        Object.values(types).map((constructor) => [constructor.name, constructor]),
      ),
    },
    querySelector(selector) {
      return record(selector, select(selector).slice(0, 1))[0] ?? null;
    },
    querySelectorAll(selector) {
      return record(selector, select(selector));
    },
  };
  return { document, nodes, queries, selected, HTMLElement, types };
}

test('the static application boundary resolves actual markup with one query per element', () => {
  const f = fixture();
  const elements = queryElements(f.document);
  assert.equal(
    elements.nameInput,
    f.nodes.find((element) => element.id === 'name-input'),
  );
  assert.equal(
    elements.loginModal,
    f.nodes.find((element) => element.id === 'login-modal'),
  );
  assert.equal(
    elements.chatInputRow,
    f.nodes.find((element) => element.id === 'chat-input-row'),
  );
  assert.equal(
    elements.roomActions,
    f.nodes.find((element) => element.id === 'room-actions'),
  );
  assert.equal(
    elements.loginAltActions,
    f.nodes.find((element) => element.className === 'auth-alt-actions'),
    'recovery actions use the login host, not the similar registration host',
  );
  assert.equal(elements.sidebarTabs.length, 3);
  assert.equal(elements.tabContents.length, 4);
  assert.equal(
    elements.lobbyTab,
    elements.sidebarTabs.find((element) => element.id === 'lobby-tab'),
  );
  assert.equal(
    elements.settingsPanel,
    elements.tabContents.find((element) => element.id === 'settings-panel'),
  );
  assert.ok([...f.queries.values()].every((count) => count === 1));
  assert.ok([...f.selected.values()].every((count) => count === 1));
  for (const name of ['classic-users-panel', 'local-tile', 'mod-menu', 'room-recovery-notice'])
    assert.equal(f.queries.has(`#${name}`), false, `${name} belongs to its dynamic owner`);
});

test('every required static ID reports its name when missing', () => {
  const initial = fixture();
  queryElements(initial.document);
  for (const original of initial.selected.keys()) {
    if (!original.id) continue;
    const f = fixture();
    const element = f.nodes.find((node) => node.id === original.id);
    element.id = '';
    assert.throws(
      () => queryElements(f.document),
      (error) =>
        error instanceof Error &&
        error.message.startsWith('Could not start simplestChat:') &&
        error.message.includes(`#${original.id}`) &&
        error.message.includes('missing'),
      original.id,
    );
  }
});

test('wrong element types fail at the boundary with the selector and expected type', () => {
  for (const id of [
    'name-input',
    'join-btn',
    'chat-input',
    'chat-input-row',
    'room-actions',
    'login-modal',
    'room-list',
  ]) {
    const f = fixture();
    const element = f.nodes.find((node) => node.id === id);
    Object.setPrototypeOf(element, f.HTMLElement.prototype);
    assert.throws(() => queryElements(f.document), new RegExp(`#${id} must be HTML\\w+Element`));
  }
  const f = fixture();
  Object.setPrototypeOf(
    f.nodes.find((node) => node.dataset.tab === 'chat'),
    f.HTMLElement.prototype,
  );
  assert.throws(() => queryElements(f.document), /#sidebar-tabs \.tab must be HTMLButtonElement/);
});

test('the nested login action host is required and typed independently of registration', () => {
  for (const wrongType of [false, true]) {
    const f = fixture();
    const host = f.nodes.find((element) => element.className === 'auth-alt-actions');
    if (wrongType) Object.setPrototypeOf(host, f.HTMLElement.prototype);
    else host.className = '';
    assert.throws(
      () => queryElements(f.document),
      wrongType
        ? /#login-modal \.auth-alt-actions must be HTMLDivElement/
        : /required element #login-modal \.auth-alt-actions is missing/,
    );
  }
});

test('a detached document reports one startup failure before querying elements', () => {
  const f = fixture();
  f.document.defaultView = null;
  assert.throws(() => queryElements(f.document), /Could not start simplestChat:.*browser window/);
  assert.equal(f.queries.size, 0);
});
