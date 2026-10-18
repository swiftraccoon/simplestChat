import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { flush, uiFixture } from './ui-fixture.mjs';

/** The text-only DOM gains the few element APIs the sheet relies on. */
async function sheetFixture({
  sections = [],
  footer = [],
  host,
  onNavigate,
  roomContext,
  onClose,
  onBack,
} = {}) {
  const f = await uiFixture();
  const { Node, document } = f;
  document.activeElement = null;
  Object.defineProperty(Node.prototype, 'classList', {
    configurable: true,
    get() {
      const node = this;
      const names = () => (node.className ?? '').split(/\s+/).filter(Boolean);
      return {
        add: (...values) => {
          node.className = [...new Set([...names(), ...values])].join(' ');
        },
        remove: (...values) => {
          node.className = names()
            .filter((name) => !values.includes(name))
            .join(' ');
        },
        toggle: (value, force) => {
          const has = names().includes(value);
          const next = force ?? !has;
          if (next && !has) node.className = [...names(), value].join(' ');
          if (!next && has)
            node.className = names()
              .filter((n) => n !== value)
              .join(' ');
          return next;
        },
        contains: (value) => names().includes(value),
      };
    },
  });
  Node.prototype.focus = function focus() {
    document.activeElement = this;
  };
  Node.prototype.contains = function contains(node) {
    for (let current = node; current; current = current.parentNode)
      if (current === this) return true;
    return false;
  };
  Object.defineProperty(Node.prototype, 'dataset', {
    configurable: true,
    get() {
      this._dataset ??= {};
      return this._dataset;
    },
  });
  const timers = [];
  const hostNode = host ?? document.createElement('div');
  document.body.append(hostNode);
  const log = [];
  const sheetModule = await loadTypeScript('src/settings-sheet.ts', {
    modules: { './ui': f.ui, './settings-sheet.css': {} },
    globals: {
      document,
      setTimeout: (callback, delay) => {
        timers.push({ callback, delay });
        return timers.length;
      },
      clearTimeout: () => {},
    },
  });
  const sheet = new sheetModule.SettingsSheet({
    sections: () => sections,
    footer: () => footer,
    host: () => hostNode,
    onOpen: () => log.push('open'),
    onClose: () => {
      log.push('close');
      onClose?.();
    },
    onNavigate,
    roomContext,
    onBack,
  });
  return { ...f, sheet, sheetModule, hostNode, log, timers };
}

const rows = (node) =>
  node.querySelectorAll('button').filter((b) => b.className?.includes('sheet-row'));
const byLabel = (node, label) =>
  rows(node).find((row) => row.querySelector('.sheet-row-label')?.textContent === label);
const heading = (sheet) => sheet.element.querySelector('h2');

test('opening lists every available section with its status and focuses the heading', async () => {
  let cameraStatus = 'FaceTime HD';
  let owner = false;
  const f = await sheetFixture({
    sections: [
      {
        id: 'camera',
        segment: 'you',
        label: 'Camera & mic',
        status: () => cameraStatus,
        screen: () => ({ title: 'Camera & mic', body: () => f.document.createElement('div') }),
      },
      {
        id: 'access',
        segment: 'room',
        label: 'Who can enter',
        available: () => owner,
        screen: () => ({ title: 'Who can enter', body: () => f.document.createElement('div') }),
      },
    ],
    footer: [{ label: 'Diagnostics', activate: () => {} }],
  });
  const opener = f.document.createElement('button');
  f.sheet.open({ opener });
  assert.equal(f.sheet.element.parentNode, f.hostNode, 'the sheet mounts into the host');
  assert.equal(f.sheet.element.hidden, false);
  assert.equal(f.sheet.element.getAttribute('role'), 'region');
  assert.equal(f.sheet.element.getAttribute('aria-label'), 'Settings');
  assert.equal(heading(f.sheet).textContent, 'Settings');
  assert.equal(f.document.activeElement, heading(f.sheet), 'focus starts on the heading');
  assert.deepEqual(
    rows(f.sheet.element).map((row) => row.querySelector('.sheet-row-label').textContent),
    ['Camera & mic'],
    'an unavailable section has no row',
  );
  assert.equal(
    byLabel(f.sheet.element, 'Camera & mic').querySelector('.sheet-row-status').textContent,
    'FaceTime HD',
  );
  assert.equal(
    f.sheet.element.querySelector('.sheet-segments'),
    null,
    'one segment needs no switch',
  );
  assert.deepEqual(f.log, ['open']);
  owner = true;
  cameraStatus = 'Elgato';
  f.sheet.refresh();
  assert.ok(f.sheet.element.querySelector('.sheet-segments'), 'two segments show the switch');
  assert.equal(
    byLabel(f.sheet.element, 'Camera & mic').querySelector('.sheet-row-status').textContent,
    'Elgato',
  );
  assert.equal(
    f.sheet.element.querySelector('.sheet-footer').textContent.includes('Diagnostics'),
    true,
  );
});

test('a row pushes its screen, Escape pops back to that row, and Escape at the root closes to the opener', async () => {
  const left = [];
  const f = await sheetFixture({
    sections: [
      {
        id: 'speaker',
        segment: 'you',
        label: 'Speaker',
        status: () => 'System default',
        screen: () => ({
          title: 'Speaker',
          body: () => {
            const body = f.document.createElement('div');
            body.append(f.document.createElement('select'));
            return body;
          },
          leave: () => left.push('speaker'),
        }),
      },
    ],
  });
  const opener = f.document.createElement('button');
  f.document.body.append(opener);
  f.sheet.open({ opener });
  const row = byLabel(f.sheet.element, 'Speaker');
  row.click();
  await flush();
  assert.equal(heading(f.sheet).textContent, 'Speaker');
  assert.equal(f.document.activeElement, heading(f.sheet), 'a pushed screen focuses its heading');
  assert.ok(f.sheet.element.querySelector('.sheet-back'), 'a detail screen offers Back');
  assert.ok(f.sheet.element.querySelector('select'), 'the screen body is shown');
  f.sheet.element.emit('keydown', { key: 'Escape', preventDefault() {} });
  await flush();
  assert.deepEqual(left, ['speaker'], 'leaving a screen runs its cleanup');
  assert.equal(heading(f.sheet).textContent, 'Settings');
  assert.equal(
    f.document.activeElement?.querySelector('.sheet-row-label')?.textContent,
    'Speaker',
    'popping focuses the row that opened the screen',
  );
  f.sheet.element.emit('keydown', { key: 'Escape', preventDefault() {} });
  assert.equal(f.sheet.element.hidden, true);
  assert.equal(f.sheet.isOpen, false);
  assert.equal(f.document.activeElement, opener, 'closing restores the opener');
  assert.deepEqual(f.log, ['open', 'close']);
});

test('closing while a screen is open runs its cleanup once and reopening starts at the root', async () => {
  const left = [];
  const f = await sheetFixture({
    sections: [
      {
        id: 'layout',
        segment: 'you',
        label: 'Layout',
        screen: () => ({
          title: 'Layout',
          body: () => f.document.createElement('div'),
          leave: () => left.push('layout'),
        }),
      },
    ],
  });
  f.sheet.open();
  byLabel(f.sheet.element, 'Layout').click();
  await flush();
  f.sheet.close();
  f.sheet.close();
  assert.deepEqual(left, ['layout']);
  f.sheet.open({ section: 'layout' });
  await flush();
  assert.equal(heading(f.sheet).textContent, 'Layout', 'opening a section pushes it directly');
  f.sheet.close();
  assert.deepEqual(left, ['layout', 'layout']);
});

test('a slow screen shows a placeholder and drops a body that arrives after leaving', async () => {
  let resolveBody;
  const f = await sheetFixture({
    sections: [
      {
        id: 'people',
        segment: 'room',
        label: 'People',
        screen: () => ({
          title: 'People',
          body: () =>
            new Promise((resolve) => {
              resolveBody = resolve;
            }),
        }),
      },
    ],
  });
  f.sheet.open();
  byLabel(f.sheet.element, 'People').click();
  await flush();
  assert.equal(f.sheet.element.querySelector('.sheet-loading')?.textContent, 'Loading…');
  f.sheet.pop();
  const late = f.document.createElement('div');
  late.className = 'late-body';
  resolveBody(late);
  await flush();
  assert.equal(f.sheet.element.querySelector('.late-body'), null, 'a stale body never renders');
  assert.equal(heading(f.sheet).textContent, 'Settings');
});

test('row primitives apply on change and mark themselves saved for a moment', async () => {
  const f = await sheetFixture();
  const { toggleRow, segmentRow, selectRow, markSaved } = f.sheetModule;
  const toggled = [];
  const toggle = toggleRow('Sounds', true, (value) => toggled.push(value), { id: 'pref-sounds' });
  assert.equal(toggle.input.type, 'checkbox');
  assert.equal(toggle.input.getAttribute('role'), 'switch');
  assert.equal(toggle.input.id, 'pref-sounds');
  assert.equal(toggle.input.checked, true);
  toggle.input.checked = false;
  toggle.input.emit('change');
  assert.deepEqual(toggled, [false]);
  const chosen = [];
  const segment = segmentRow(
    'Talk',
    [
      { value: 'open', label: 'Open mic', id: 'talk-open' },
      { value: 'ptt', label: 'Hold to talk', id: 'talk-ptt' },
    ],
    'open',
    (value) => chosen.push(value),
  );
  const buttons = segment.row.querySelectorAll('button');
  assert.deepEqual(
    buttons.map((b) => b.getAttribute('aria-pressed')),
    ['true', 'false'],
  );
  buttons[1].click();
  assert.deepEqual(chosen, ['ptt']);
  assert.deepEqual(
    buttons.map((b) => b.getAttribute('aria-pressed')),
    ['false', 'true'],
    'the pressed state follows the choice',
  );
  segment.set('open');
  assert.equal(buttons[0].getAttribute('aria-pressed'), 'true');
  const select = f.document.createElement('select');
  const picked = selectRow('Layout', select);
  assert.ok(picked.querySelector('select'));
  markSaved(toggle.row);
  assert.equal(toggle.row.querySelector('.sheet-row-saved')?.textContent, 'Saved');
  f.timers.at(-1).callback();
  assert.equal(toggle.row.querySelector('.sheet-row-saved')?.textContent, '', 'the mark clears');
});

test('the segment switch remembers the chosen segment across screens', async () => {
  const f = await sheetFixture({
    sections: [
      {
        id: 'camera',
        segment: 'you',
        label: 'Camera & mic',
        screen: () => ({ title: 'Camera & mic', body: () => f.document.createElement('div') }),
      },
      {
        id: 'access',
        segment: 'room',
        label: 'Who can enter',
        screen: () => ({ title: 'Who can enter', body: () => f.document.createElement('div') }),
      },
    ],
  });
  f.sheet.open();
  assert.deepEqual(
    rows(f.sheet.element).map((r) => r.querySelector('.sheet-row-label').textContent),
    ['Camera & mic'],
  );
  const segments = f.sheet.element.querySelector('.sheet-segments').querySelectorAll('button');
  segments[1].click();
  assert.deepEqual(
    rows(f.sheet.element).map((r) => r.querySelector('.sheet-row-label').textContent),
    ['Who can enter'],
  );
  byLabel(f.sheet.element, 'Who can enter').click();
  await flush();
  f.sheet.pop();
  assert.deepEqual(
    rows(f.sheet.element).map((r) => r.querySelector('.sheet-row-label').textContent),
    ['Who can enter'],
    'the room segment stays selected',
  );
  f.sheet.close();
  f.sheet.open({ segment: 'you' });
  assert.deepEqual(
    rows(f.sheet.element).map((r) => r.querySelector('.sheet-row-label').textContent),
    ['Camera & mic'],
  );
});

test('a screen that cannot be left yet keeps the sheet open on Escape, Back and close', async () => {
  let busy = true;
  const left = [];
  const f = await sheetFixture({
    sections: [
      {
        id: 'security',
        segment: 'you',
        label: 'Sign-in & security',
        screen: () => ({
          title: 'Sign-in & security',
          body: () => f.document.createElement('div'),
          canLeave: () => !busy,
          leave: () => left.push('security'),
        }),
      },
    ],
  });
  f.sheet.open();
  byLabel(f.sheet.element, 'Sign-in & security').click();
  await flush();
  f.sheet.element.emit('keydown', { key: 'Escape', preventDefault() {} });
  f.sheet.close();
  f.sheet.element.querySelector('.sheet-back').click();
  assert.equal(heading(f.sheet).textContent, 'Sign-in & security', 'the screen stays');
  assert.equal(f.sheet.isOpen, true);
  assert.deepEqual(left, []);
  busy = false;
  f.sheet.close();
  assert.equal(f.sheet.isOpen, false);
  assert.deepEqual(left, ['security']);
});

test('an identity change forces the sheet closed even while a screen is busy', async () => {
  const left = [];
  const f = await sheetFixture({
    sections: [
      {
        id: 'security',
        segment: 'you',
        label: 'Sign-in & security',
        screen: () => ({
          title: 'Sign-in & security',
          body: () => f.document.createElement('div'),
          canLeave: () => false,
          leave: () => left.push('security'),
        }),
      },
    ],
  });
  f.sheet.open();
  byLabel(f.sheet.element, 'Sign-in & security').click();
  await flush();
  f.sheet.close();
  assert.equal(f.sheet.isOpen, true, 'an ordinary close waits for the screen');
  f.sheet.close({ force: true });
  assert.equal(f.sheet.isOpen, false);
  assert.equal(f.sheet.element.hidden, true);
  assert.deepEqual(left, ['security'], 'the busy screen still runs its cleanup');
});

test('row inputs are named by their label alone, so a Saved mark never renames them', async () => {
  const f = await sheetFixture();
  const { fieldRow, toggleRow, selectRow, markSaved } = f.sheetModule;
  const name = f.document.createElement('input');
  const field = fieldRow('Name', name);
  const toggle = toggleRow('Mentions', true, () => {});
  const select = f.document.createElement('select');
  const picked = selectRow('Speaker', select);
  for (const [row, input, label] of [
    [field, name, 'Name'],
    [toggle.row, toggle.input, 'Mentions'],
    [picked, select, 'Speaker'],
  ]) {
    const labelNode = row.querySelector('.sheet-row-label');
    assert.ok(labelNode.id, `${label} gets an id for its input`);
    assert.equal(input.getAttribute('aria-labelledby'), labelNode.id);
    markSaved(row);
    assert.equal(labelNode.textContent, label, 'the label text is untouched by the mark');
    assert.equal(row.querySelector('.sheet-row-saved')?.textContent, 'Saved');
  }
  assert.notEqual(
    field.querySelector('.sheet-row-label').id,
    toggle.row.querySelector('.sheet-row-label').id,
    'ids are unique per row',
  );
});

test('closing falls back to the control that was active at opening when the opener cannot take focus', async () => {
  const f = await sheetFixture({
    sections: [
      {
        id: 'profile',
        segment: 'you',
        label: 'Profile',
        screen: () => ({ title: 'Profile', body: () => f.document.createElement('div') }),
      },
    ],
  });
  // A menu focuses its toggle before an item runs, so the item itself is hidden by the time
  // the sheet closes.
  const toggle = f.document.createElement('button');
  f.document.body.append(toggle);
  toggle.focus();
  const hiddenItem = f.document.createElement('button');
  f.sheet.open({ section: 'profile', opener: hiddenItem });
  await flush();
  assert.equal(heading(f.sheet).textContent, 'Profile');
  f.sheet.close();
  assert.equal(f.document.activeElement, toggle, 'focus returns to the visible toggle');
  const visibleOpener = f.document.createElement('button');
  f.document.body.append(visibleOpener);
  toggle.focus();
  f.sheet.open({ opener: visibleOpener });
  f.sheet.close();
  assert.equal(f.document.activeElement, visibleOpener, 'a connected opener still wins');
});

test('navigation snapshots restore a section and child screen while retiring old async work', async () => {
  const navigation = [];
  const contexts = [];
  let cleanups = 0;
  const f = await sheetFixture({
    onNavigate: (state) => navigation.push(state),
    sections: [
      {
        id: 'privacy',
        segment: 'you',
        label: 'Privacy',
        screen: () => ({
          title: 'Privacy',
          body: (context) => {
            contexts.push(context);
            return f.document.createElement('div');
          },
          leave: () => {
            cleanups++;
          },
        }),
      },
    ],
  });
  f.sheet.open({ section: 'privacy' });
  assert.equal(navigation.length, 2);
  assert.equal(navigation.at(-1).section, 'privacy');
  const section = navigation.at(-1);
  f.sheet.push({ title: 'Ignored account', body: () => f.document.createElement('div') });
  const child = navigation.at(-1);
  f.sheet.close();
  assert.equal(navigation.at(-1), null);
  assert.equal(contexts[0].current(), false);
  section.restore();
  assert.equal(heading(f.sheet).textContent, 'Privacy');
  assert.equal(contexts.at(-1).current(), true);
  child.restore();
  assert.equal(heading(f.sheet).textContent, 'Ignored account');
  assert.equal(cleanups, 2);
});

test('a restored section must still be available for the current account and room', async () => {
  let builds = 0;
  const f = await sheetFixture({
    sections: [
      {
        id: 'room-members',
        segment: 'room',
        label: 'People',
        available: () => false,
        screen: () => {
          builds++;
          return { title: 'People', body: () => f.document.createElement('div') };
        },
      },
    ],
  });
  f.sheet.open({ section: 'room-members' });
  assert.equal(builds, 0);
  assert.equal(heading(f.sheet).textContent, 'Settings');
});

test('Forward rebuilds a section factory rather than reviving an aborted controller', async () => {
  const navigation = [];
  const rendered = [];
  const f = await sheetFixture({
    onNavigate: (state) => navigation.push(state),
    sections: [
      {
        id: 'security',
        segment: 'you',
        label: 'Security',
        screen: () => {
          let retired = false;
          return {
            title: 'Security',
            leave: () => {
              retired = true;
            },
            body: () => {
              rendered.push(retired);
              return f.document.createElement('div');
            },
          };
        },
      },
    ],
  });
  f.sheet.open({ section: 'security' });
  const snapshot = navigation.at(-1);
  f.sheet.close();
  snapshot.restore();
  assert.deepEqual(rendered, [false, false]);
});

test('a retired managed-room snapshot never opens the live room with the same section ID', async () => {
  const navigation = [];
  let scope = 'managed:B';
  const builds = [];
  const f = await sheetFixture({
    roomContext: () => scope,
    onClose: () => {
      scope = 'live:A';
    },
    onNavigate: (state) => navigation.push(state),
    sections: [
      {
        id: 'room-access',
        segment: 'room',
        label: 'Who can enter',
        screen: () => {
          builds.push(scope);
          return { title: 'Who can enter', body: () => f.document.createElement('div') };
        },
      },
    ],
  });
  f.sheet.open({ section: 'room-access' });
  const saved = navigation.at(-1);
  f.sheet.close();
  saved.restore();
  assert.equal(heading(f.sheet).textContent, 'Settings');
  assert.equal(navigation.at(-1).section, undefined);
  assert.deepEqual(builds, ['managed:B']);
  scope = 'managed:C';
  f.sheet.open({ section: 'room-access' });
  saved.restore();
  assert.equal(scope, 'live:A', 'a different selected room is cleared before the safe root');
  assert.equal(heading(f.sheet).textContent, 'Settings');
  assert.deepEqual(builds, ['managed:B', 'managed:C']);
});

test('source-integrated cold settings Account to Profile to Back to Security then Close remains closed', async () => {
  const location = {
    pathname: '/',
    search: '',
    hash: '#view=settings&section=security&segment=you',
  };
  const entries = [{ hash: location.hash, state: null }];
  let cursor = 0;
  const history = {
    state: null,
    pushState(state, _title, url) {
      entries.splice(cursor + 1);
      entries.push({ hash: new URL(url, 'https://test.invalid').hash, state });
      cursor++;
      this.state = state;
      location.hash = entries[cursor].hash;
    },
    replaceState(state, _title, url) {
      entries[cursor] = { hash: new URL(url, 'https://test.invalid').hash, state };
      this.state = state;
      location.hash = entries[cursor].hash;
    },
    go(delta) {
      cursor = Math.max(0, Math.min(entries.length - 1, cursor + delta));
      this.state = entries[cursor].state;
      location.hash = entries[cursor].hash;
      navigation.locationChanged('');
    },
  };
  let stop;
  const f = await sheetFixture({
    sections: ['profile', 'security'].map((id) => ({
      id,
      segment: 'you',
      label: id,
      screen: () => ({ title: id, body: () => f.document.createElement('div') }),
    })),
    onBack: () => navigation.back(),
    onNavigate: (state, action) => {
      if (!state) {
        stop?.();
        stop = undefined;
        return;
      }
      stop = navigation.opened(
        {
          group: 'settings',
          accountId: 'account',
          destination: {
            kind: 'settings',
            ...(state.section && { section: state.section }),
            segment: state.segment,
          },
          canLeave: state.canLeave,
          close: () => f.sheet.close(),
          restore: state.restore,
        },
        action === 'pop',
      );
    },
  });
  const { AppNavigation } = await loadTypeScript('src/app-navigation.ts', {
    globals: { window: { location, history }, URLSearchParams },
  });
  const navigation = new AppNavigation({
    getRoom: () => '',
    getAccountId: () => 'account',
    ready: () => true,
    restore: (destination) =>
      f.sheet.open({ section: destination.section, segment: destination.segment }),
    error: (message) => assert.fail(message),
  });
  navigation.synchronize();
  assert.equal(heading(f.sheet).textContent, 'security');
  f.sheet.open({ section: 'profile' });
  f.sheet.pop();
  assert.equal(heading(f.sheet).textContent, 'Settings');
  byLabel(f.sheet.element, 'security').click();
  assert.equal(heading(f.sheet).textContent, 'security');
  f.sheet.close();
  assert.equal(f.sheet.isOpen, false);
  assert.equal(location.hash, '');
});
