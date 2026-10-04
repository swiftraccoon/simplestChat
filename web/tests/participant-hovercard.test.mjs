import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './source-loader.mjs';
import { createDOM, deferred, flush } from './ui-fixture.mjs';
import { loadAppearanceFixture } from './appearance-fixture.mjs';

async function fixture() {
  const { document, Node } = createDOM();
  const timers = new Map();
  const observers = [];
  let timerId = 0;
  const events = new Node('events');
  document.addEventListener = events.addEventListener.bind(events);
  document.documentElement = { clientWidth: 320 };
  document.activeElement = document.body;
  const originalCreate = document.createElement;
  document.createElement = (tag) => {
    const node = originalCreate(tag);
    node.dataset = {};
    return node;
  };
  Node.prototype.contains = function (target) {
    return target === this || this.children.some((child) => child.contains(target));
  };
  Node.prototype.focus = function () {
    const previous = document.activeElement;
    document.activeElement = this;
    if (previous !== this) {
      previous?.emit('focusout');
      this.emit('focus');
    }
  };
  Node.prototype.getBoundingClientRect = function () {
    return this.className === 'participant-hovercard'
      ? { left: 0, top: 0, right: 280, bottom: 180, width: 280, height: 180 }
      : { left: 290, top: 440, right: 310, bottom: 460, width: 20, height: 20 };
  };
  Object.defineProperty(Node.prototype, 'childElementCount', {
    get() {
      return this.children.length;
    },
  });
  const ui = {
    el(tag, text, className) {
      const node = document.createElement(tag);
      if (text !== undefined) node.textContent = text;
      if (className) node.className = className;
      return node;
    },
    button(label, action) {
      const node = ui.el('button', label);
      node.addEventListener('click', action);
      return node;
    },
    safeRasterUrl: (value) =>
      typeof value === 'string' && value.startsWith('data:image/png;base64,'),
  };
  const state = {
    session: {},
    data: { id: 'alice', name: 'Alice', online: true, self: false, profileAvailable: true },
    profiles: [],
    actions: [],
  };
  const { ParticipantHovercard } = await loadTypeScript('src/participant-hovercard.ts', {
    modules: {
      './ui': ui,
      './avatar-colors': await loadTypeScript('src/avatar-colors.ts'),
      './appearance': await loadAppearanceFixture({ Node, ui }),
    },
    globals: {
      Node,
      document,
      window: { innerHeight: 480, addEventListener: events.addEventListener.bind(events) },
      MutationObserver: class {
        observing = false;
        constructor(callback) {
          this.callback = callback;
          observers.push(this);
        }
        observe() {
          this.observing = true;
        }
        disconnect() {
          this.observing = false;
        }
      },
      setTimeout(callback) {
        timers.set(++timerId, callback);
        return timerId;
      },
      clearTimeout: (id) => timers.delete(id),
    },
  });
  const controller = new ParticipantHovercard({
    getSession: () => state.session,
    getParticipant: () => state.data,
    loadProfile: () => {
      const pending = deferred();
      state.profiles.push(pending);
      return pending.promise;
    },
    onMessage: (...args) => state.actions.push(args),
    onProfile: (...args) => state.actions.push(args),
  });
  const anchor = ui.el('button', 'Alice');
  document.body.append(anchor);
  controller.bind(anchor, 'alice', 'Alice');
  const card = document.querySelector('.participant-hovercard');
  const tick = () => {
    const pending = [...timers.values()];
    timers.clear();
    pending.forEach((callback) => callback());
  };
  return { controller, anchor, card, document, state, timers, events, tick, observers };
}

test('hover waits, pointer transfer retains the card, and quick exits cancel opening', async () => {
  const f = await fixture();
  f.anchor.emit('pointerenter', { pointerType: 'mouse' });
  assert.equal(f.card.hidden, true);
  f.anchor.emit('pointerleave');
  f.tick();
  assert.equal(f.card.hidden, true);
  f.anchor.emit('pointerenter', { pointerType: 'mouse' });
  f.tick();
  assert.equal(f.card.hidden, false);
  assert.equal(f.document.activeElement, f.document.body);
  f.anchor.emit('pointerleave');
  f.card.emit('pointerenter');
  f.tick();
  assert.equal(f.card.hidden, false);
  f.card.emit('pointerleave');
  f.tick();
  assert.equal(f.card.hidden, true);
});

test('room changes cancel queued opening and late profile writes', async () => {
  const f = await fixture();
  f.anchor.emit('pointerenter', { pointerType: 'mouse' });
  f.state.session = {};
  f.tick();
  assert.equal(f.card.hidden, true);
  f.anchor.focus();
  const pending = f.state.profiles[0];
  f.controller.reset();
  pending.resolve({ bio: 'Must not appear', displayName: 'Old account' });
  await flush();
  assert.equal(f.card.hidden, true);
  assert.equal(f.card.textContent, '');
  assert.equal(f.timers.size, 0);
});

test('profile completions are guarded even before a changed session is refreshed', async () => {
  const f = await fixture();
  f.anchor.focus();
  f.state.session = {};
  f.state.profiles[0].resolve({ bio: 'Stale profile' });
  await flush();
  assert.equal(f.card.textContent.includes('Stale profile'), false);
  f.controller.refresh();
  assert.equal(f.card.hidden, true);
});

test('keyboard entry and Escape restore focus without reopening or fetching again', async () => {
  const f = await fixture();
  f.anchor.focus();
  let prevented = false;
  f.anchor.emit('keydown', {
    key: 'Tab',
    shiftKey: false,
    preventDefault: () => (prevented = true),
  });
  assert.equal(prevented, true);
  assert.equal(f.document.activeElement.textContent, 'Profile');
  f.events.emit('keydown', { key: 'Escape', preventDefault() {}, stopPropagation() {} });
  assert.equal(f.document.activeElement, f.anchor);
  assert.equal(f.card.hidden, true);
  assert.equal(f.state.profiles.length, 1);
});

test('profile content stays plain text, images are raster-only, and placement stays onscreen', async () => {
  const f = await fixture();
  f.anchor.focus();
  f.state.profiles[0].resolve({
    displayName: '<b>Alice</b>',
    bio: '<script>plain text</script>',
    avatarUrl: 'https://external.example/avatar.svg',
  });
  await flush();
  assert.equal(
    f.card.querySelector('.participant-hovercard-profile-name').textContent,
    '<b>Alice</b>',
  );
  assert.equal(
    f.card.querySelector('.participant-hovercard-bio').textContent,
    '<script>plain text</script>',
  );
  assert.equal(f.card.querySelector('img'), null);
  assert.equal(f.card.style.left, '32px');
  assert.equal(f.card.style.top, '254px');
});

test('guests do not load account profiles and stale card actions cannot run', async () => {
  const f = await fixture();
  f.state.data = { ...f.state.data, profileAvailable: false, canMessage: true };
  f.anchor.focus();
  assert.equal(f.state.profiles.length, 0);
  assert.equal(f.card.querySelector('.participant-hovercard-profile-name').parentNode.hidden, true);
  const action = f.card.querySelector('button');
  assert.equal(action.textContent, 'Message');
  f.state.session = {};
  action.click();
  assert.deepEqual(f.state.actions, []);
  assert.equal(f.card.hidden, true);
});

test('nickname and account name remain explicitly labelled when they match', async () => {
  const f = await fixture();
  f.anchor.focus();
  f.state.profiles[0].resolve({ displayName: 'Alice' });
  await flush();
  assert.deepEqual(
    f.card.querySelectorAll('.participant-hovercard-label').map((node) => node.textContent),
    ['Nickname', 'Account name'],
  );
  const accountName = f.card.querySelector('.participant-hovercard-profile-name');
  assert.equal(accountName.textContent, 'Alice');
  assert.equal(accountName.parentNode.hidden, false);
});

test('profile appearance and avatar fallback are independent of the chat color', async () => {
  const f = await fixture();
  f.state.data.color = 'red';
  f.anchor.focus();
  const avatar = f.card.querySelector('.participant-hovercard-avatar');
  const content = f.card.querySelector('.appearance-custom');
  assert.notEqual(avatar.style.background, '#f87171');
  f.state.profiles[0].resolve({
    displayName: 'Alice',
    profileStyle: { color: 'teal', style: 'bubble' },
  });
  await flush();
  assert.equal(content.dataset.appearance, 'bubble');
  assert.equal(content.style['--appearance-color'], '#2dd4bf');
  assert.equal(avatar.style.background, '#2dd4bf');
  assert.equal(f.state.data.color, 'red');
});

test('roster removal closes the card and disconnected anchors cannot reopen it', async () => {
  const f = await fixture();
  f.anchor.focus();
  f.anchor.remove();
  f.controller.refresh();
  assert.equal(f.card.hidden, true);
  f.anchor.emit('pointerenter', { pointerType: 'mouse' });
  f.tick();
  assert.equal(f.card.hidden, true);
});

test('actions recheck current permissions and connection before invoking a callback', async () => {
  for (const update of [
    (f) => (f.state.data = { ...f.state.data, canMessage: false }),
    (f) => (f.state.data = { ...f.state.data, self: true }),
    (f) => (f.state.data = { ...f.state.data, online: false }),
    (f) => f.anchor.remove(),
  ]) {
    const f = await fixture();
    f.state.data = { ...f.state.data, canMessage: true };
    f.anchor.focus();
    const action = f.card.querySelector('button');
    update(f);
    action.click();
    assert.deepEqual(f.state.actions, []);
    assert.equal(f.card.hidden, true);
  }
});

test('anchor removal closes a pinned card without requiring a roster update', async () => {
  const f = await fixture();
  assert.equal(f.observers[0].observing, false);
  f.anchor.emit('click', { preventDefault() {}, stopPropagation() {} });
  assert.equal(f.observers[0].observing, true);
  f.anchor.remove();
  f.observers[0].callback();
  assert.equal(f.card.hidden, true);
  assert.equal(f.observers[0].observing, false);
  assert.equal(f.anchor.getAttribute('aria-expanded'), 'false');
});

test('keyboard navigation resumes at the trigger when leaving either edge of the card', async () => {
  for (const shiftKey of [true, false]) {
    const f = await fixture();
    f.anchor.focus();
    f.card.querySelector('button').focus();
    let prevented = false;
    f.card.emit('keydown', {
      key: 'Tab',
      shiftKey,
      preventDefault: () => (prevented = true),
    });
    assert.equal(prevented, shiftKey, 'Forward Tab retains native navigation after the trigger');
    assert.equal(f.card.hidden, true);
    assert.equal(f.document.activeElement, f.anchor);
    assert.equal(f.state.profiles.length, 1);
  }
});
