import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { roomScreenFixture } from './room-screen-fixture.mjs';
import { deferred, flush } from './ui-fixture.mjs';

async function fixture({ share = false } = {}) {
  const f = await roomScreenFixture();
  Object.defineProperty(f.Node.prototype, 'parentElement', {
    get() {
      return this.parentNode;
    },
  });
  f.Node.prototype.select = function () {
    this.selected = true;
  };
  for (const name of [
    'micSetupBtn',
    'copyRoomLink',
    'rosterToggleBtn',
    'chatToggleBtn',
    'roomMoreBtn',
    'shortcutsBtn',
    'header',
    'roomTools',
    'toastContainer',
    'roomMoreMenu',
    'accountMenuBtn',
    'accountMenu',
    'headerRight',
    'sidebarCollapseBtn',
    'homeTools',
  ]) {
    f.elements[name] = f.document.createElement(name.endsWith('Btn') ? 'button' : 'div');
    f.document.body.append(f.elements[name]);
  }
  const clipboard = deferred();
  const shared = deferred();
  const state = {
    room: { currentRoomId: 'fixture-room', roomSettings: { displayName: 'Fixture room' } },
    copies: [],
    shares: [],
    notices: [],
    errors: [],
    tasks: [],
    views: [],
  };
  const navigator = {
    mediaDevices: { getDisplayMedia() {} },
    clipboard: {
      writeText(value) {
        state.copies.push(value);
        return clipboard.promise;
      },
    },
    ...(share && {
      share(value) {
        state.shares.push(value);
        return shared.promise;
      },
    }),
  };
  f.window.location = { href: 'https://example.test/?remove=me#previous-room' };
  const module = await loadTypeScript('src/screens/chrome.ts', {
    modules: {
      '../ui': {
        ...f.ui,
        modal(...args) {
          const view = f.ui.modal(...args);
          state.views.push(view);
          return view;
        },
      },
    },
    globals: { document: f.document, window: f.window, navigator },
  });
  const services = {
    getRoom: () => state.room,
    settings: { open() {} },
    telemetry: { sharingEnabled: false, summary: () => 'Summary fixture', setSharing() {} },
    layout: { desktop: !share },
    notify: (message) => state.notices.push(message),
    observe: (task, message) =>
      state.tasks.push(task.catch((error) => state.errors.push({ error, message }))),
    asyncAction: (action, message) => () => services.observe(action(), message),
  };
  const chrome = module.mountChrome(f.elements, services);
  return {
    ...f,
    state,
    chrome,
    clipboard,
    shared,
    async settle() {
      await Promise.all(state.tasks);
      await flush();
      assert.deepEqual(state.errors, []);
    },
  };
}

for (const retirement of ['dispose', 'replacement']) {
  for (const outcome of ['resolve', 'reject']) {
    test(`room-link clipboard ${outcome} stays silent after chrome ${retirement}`, async () => {
      const f = await fixture();
      f.elements.copyRoomLink.click();
      assert.deepEqual(f.state.copies, ['https://example.test/#fixture-room']);
      if (retirement === 'dispose') f.chrome.dispose();
      else f.state.room = { ...f.state.room };
      if (outcome === 'resolve') f.clipboard.resolve();
      else f.clipboard.reject(new Error('Clipboard refused'));
      await f.settle();
      assert.deepEqual(f.state.notices, []);
      assert.deepEqual(f.state.views, [], 'late failures cannot open a copy dialog');
      f.chrome.dispose();
      f.controller.dispose();
    });
  }
  test(`a rejected room share cannot start clipboard fallback after chrome ${retirement}`, async () => {
    const f = await fixture({ share: true });
    f.elements.copyRoomLink.click();
    assert.equal(f.state.shares.length, 1);
    assert.deepEqual(f.state.copies, []);
    if (retirement === 'dispose') f.chrome.dispose();
    else f.state.room = { ...f.state.room };
    f.shared.reject(new Error('Sharing unavailable'));
    await f.settle();
    assert.deepEqual(f.state.copies, []);
    assert.deepEqual(f.state.notices, []);
    assert.deepEqual(f.state.views, []);
    f.chrome.dispose();
    f.controller.dispose();
  });
}

test('a live share rejection retains the explicit clipboard fallback and its success notice', async () => {
  const f = await fixture({ share: true });
  f.elements.copyRoomLink.click();
  f.shared.reject(new Error('Sharing unavailable'));
  await flush();
  assert.deepEqual(f.state.copies, ['https://example.test/#fixture-room']);
  f.clipboard.resolve();
  await f.settle();
  assert.deepEqual(f.state.notices, ['Room link copied']);
  f.chrome.dispose();
  f.controller.dispose();
});

for (const retirement of ['dispose', 'close']) {
  for (const outcome of ['resolve', 'reject']) {
    test(`diagnostic clipboard ${outcome} cannot notify or select after ${retirement}`, async () => {
      const f = await fixture();
      f.chrome.diagnosticsButton.click();
      const view = f.state.views[0];
      const copy = view.body
        .querySelectorAll('button')
        .find((button) => button.textContent === 'Copy summary');
      const preview = view.body.querySelector('textarea');
      copy.click();
      assert.deepEqual(f.state.copies, ['Summary fixture']);
      f.document.activeElement = f.document.body;
      if (retirement === 'dispose') f.chrome.dispose();
      else view.close();
      if (outcome === 'resolve') f.clipboard.resolve();
      else f.clipboard.reject(new Error('Clipboard refused'));
      await f.settle();
      assert.deepEqual(f.state.notices, []);
      assert.equal(preview.selected, undefined);
      assert.equal(f.document.activeElement, f.document.body);
      copy.click();
      assert.equal(f.state.copies.length, 1, 'a retired modal cannot start another copy');
      f.chrome.dispose();
      f.chrome.diagnosticsButton.click();
      assert.equal(
        f.state.views.length,
        1,
        'a disposed chrome cannot open another diagnostic dialog',
      );
      f.controller.dispose();
    });
  }
}
