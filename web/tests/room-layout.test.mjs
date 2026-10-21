import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createScope } from '../src/disposable.ts';
import { loadTypeScript } from './native-fixtures.mjs';
import { createDOM } from './ui-fixture.mjs';

async function fixture({ width = 375, landscape = false, saved = {} } = {}) {
  const dom = createDOM();
  const createElement = dom.document.createElement;
  dom.document.createElement = (tag) => {
    const node = createElement(tag);
    node.dataset = {};
    return node;
  };
  const state = { width, landscape, renders: 0, saved };
  const target = new EventTarget();
  const window = {
    get innerWidth() {
      return state.width;
    },
    matchMedia(query) {
      return {
        matches: query.includes(',') ? state.width <= 768 || state.landscape : state.landscape,
      };
    },
    addEventListener: target.addEventListener.bind(target),
    dispatchEvent: target.dispatchEvent.bind(target),
  };
  const make = (id) => {
    const node = dom.document.createElement('div');
    node.id = id;
    node.dataset = {};
    const append = node.append.bind(node);
    node.append = (...children) =>
      append(
        ...children.map((child) => {
          if (typeof child !== 'string') return child;
          const text = new dom.Node('#text');
          text.textContent = child;
          return text;
        }),
      );
    const classes = new Set();
    node.classList = {
      toggle(name, on) {
        if (on) classes.add(name);
        else classes.delete(name);
      },
      add: (...names) => names.forEach((name) => classes.add(name)),
      remove: (...names) => names.forEach((name) => classes.delete(name)),
      contains: (name) => classes.has(name),
    };
    node.style = {
      values: new Map(),
      setProperty(key, value) {
        this.values.set(key, value);
      },
    };
    node.insertAdjacentHTML = () => {};
    const add = node.addEventListener.bind(node);
    node.addEventListener = (name, callback, options = {}) =>
      add(
        name,
        (event) => {
          if (!options.signal?.aborted) callback(event);
        },
        options,
      );
    return node;
  };
  const names = [
    'roomScreen',
    'sidebarCollapseBtn',
    'lobbyTab',
    'chatTab',
    'usersTab',
    'rosterToggleBtn',
    'chatToggleBtn',
    'sidebarElement',
    'rosterToolLabel',
    'chatToolLabel',
  ];
  const elements = Object.fromEntries(names.map((name) => [name, make(name)]));
  elements.chatTab.dataset.tab = 'chat';
  elements.usersTab.dataset.tab = 'users';
  elements.lobbyTab.dataset.tab = 'lobby';
  elements.sidebarTabs = [elements.chatTab, elements.usersTab, elements.lobbyTab];
  elements.tabContents = ['chat', 'users', 'lobby', 'settings'].map((tab) => make(`${tab}-panel`));
  dom.document.body.append(elements.roomScreen);
  elements.roomScreen.append(elements.sidebarElement);
  dom.document.documentElement = { style: { setProperty() {} } };
  const preferences = new Map([['layout', 'classic']]);
  const localStorage = {
    getItem: () => JSON.stringify(state.saved),
    setItem: (_key, value) => {
      state.saved = JSON.parse(value);
    },
  };
  const module = await loadTypeScript('src/screens/room-layout.ts', {
    modules: {
      '../disposable': { createScope },
      '../icons': { chatIcon: () => '', userIcon: () => '' },
    },
    globals: { window, document: dom.document, localStorage, Element: dom.Node },
  });
  const layout = module.createRoomLayout(elements, {
    readPreference: (key) => preferences.get(key) ?? null,
    writePreference: (key, value) => preferences.set(key, value),
    renderParticipants: () => state.renders++,
  });
  return { ...dom, make, state, window, elements, layout, module };
}

test('phone panels keep their tab bar usable, restore labels and ignore desktop collapse state', async () => {
  const f = await fixture({ saved: { mobilePanelCollapsed: true } });
  const { elements, layout } = f;
  layout.apply();
  assert.equal(elements.rosterToolLabel.textContent, 'People');
  assert.equal(elements.chatToolLabel.textContent, 'Chat');
  assert.equal(elements.roomScreen.classList.contains('mobile-panel-collapsed'), true);
  assert.equal(elements.sidebarCollapseBtn.getAttribute('aria-expanded'), 'false');
  assert.equal(elements.sidebarElement.inert, false);
  f.state.width = 1200;
  layout.apply();
  assert.equal(elements.roomScreen.classList.contains('mobile-panel-collapsed'), false);
  assert.equal(elements.sidebarCollapseBtn.getAttribute('aria-expanded'), 'true');
  assert.equal(layout.preferences.rosterWidth, 220);
  assert.equal(layout.preferences.chatWidth, 320);
  elements.chatToggleBtn.click();
  assert.equal(elements.sidebarElement.inert, true);
  assert.equal(elements.chatToolLabel.textContent, 'Show chat');
});

test('a landscape phone uses the people tab, and desktop CSS predicates remain in step', async () => {
  const f = await fixture({ width: 844, landscape: true });
  f.layout.apply();
  assert.equal(f.elements.usersTab.hidden, false);
  assert.equal(f.layout.mobileCollapsed, false);
  f.state.width = 1200;
  f.state.landscape = false;
  f.layout.apply();
  assert.equal(f.elements.usersTab.hidden, true);
  const css = await readFile(new URL('../src/style.css', import.meta.url), 'utf8');
  const queries = [];
  const module = await loadTypeScript('src/screens/room-layout.ts', {
    modules: { '../disposable': { createScope }, '../icons': {} },
    globals: {
      window: {
        matchMedia: (query) => {
          queries.push(query);
          return { matches: false };
        },
      },
    },
  });
  assert.equal(module.isDesktopLayout(), true);
  for (const part of queries[0].split(',').map((query) => query.trim()))
    assert.ok(css.includes(`@media ${part} {`), `CSS lacks ${part}`);
});

test('panel controls and viewport listeners retire together and a replacement roster has one resize handle', async () => {
  const f = await fixture({ width: 1200 });
  const panel = f.make('roster');
  f.layout.attachPanelResize(panel, 'roster');
  f.layout.attachPanelResize(panel, 'roster');
  assert.equal(panel.querySelectorAll('.panel-resize-handle').length, 1);
  f.state.width = 400;
  f.window.dispatchEvent(new Event('resize'));
  assert.equal(f.state.renders, 1);
  f.layout.dispose();
  f.layout.dispose();
  f.state.width = 1200;
  f.window.dispatchEvent(new Event('resize'));
  f.elements.chatToggleBtn.click();
  assert.equal(f.state.renders, 1);
  assert.equal(f.layout.preferences.chatCollapsed, false);
});
