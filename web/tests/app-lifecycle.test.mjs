import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';
import { createDOM } from './ui-fixture.mjs';

const ui = await loadTypeScript('src/ui.ts');
const auth = await loadTypeScript('src/auth.ts');
const mediaControls = await loadTypeScript('src/media-controls.ts');

async function fixture(failureStage, cleanupFailure = null) {
  const { Node } = createDOM();
  Node.prototype.before = function (node) {
    this.parentNode?.insertBefore(node, this);
  };
  Node.prototype.prepend = function (node) {
    this.insertBefore(node, this.firstChild);
  };
  const parent = new Node('header');
  const qualityIndicator = new Node('span');
  parent.append(qualityIndicator);
  const window = new EventTarget();
  const released = [];
  const failure = new Error('fixture constructor failed');
  let telemetryEvents = 0;
  const release = (name) => {
    window.dispatchEvent(new Event('error'));
    released.push(name);
    if (name === 'chrome' && cleanupFailure) throw cleanupFailure;
  };
  const controller = (name) => ({ dispose: () => release(name) });
  const failAt = (stage) => {
    if (failureStage === stage) throw failure;
  };
  if (failureStage === 'adopt-quality') {
    qualityIndicator.setAttribute = () => {
      throw failure;
    };
  }
  const { startApp } = await loadTypeScript('src/app.ts', {
    globals: { window, document: {} },
    modules: {
      './ui': { ...ui, button: () => new Node('button') },
      './telemetry': {
        ClientTelemetry: class {
          record = () => telemetryEvents++;
          dispose() {
            release('telemetry');
          }
        },
      },
      './video-layout': { observeVideoLayout: () => () => release('video-layout') },
      './toasts': {
        createToasts: () => ({
          ...controller('toasts'),
          showToast() {},
          showActionToast() {},
        }),
      },
      './ui-tasks': {
        createUiTasks: () => ({ observeUiTask() {}, asyncUiAction() {} }),
      },
      './screens/chrome': {
        mountChrome: () => {
          failAt('chrome');
          return controller('chrome');
        },
      },
      './auth': {
        ...auth,
        AuthManager: class {
          setTelemetryHandler() {}
          setOnChange() {}
          dispose() {
            release('auth');
          }
        },
      },
      './screens/auth': {
        AuthScreen: class {
          dispose() {
            release('auth-screen');
          }
        },
      },
      './screens/home': {
        createHomeScreen: () => ({
          ...controller('home'),
          updateAuthUI() {},
          updateJoinBtn() {},
        }),
      },
      './screens/invitations': {
        createInvitations: () => ({ ...controller('invitations'), readInviteLink() {} }),
      },
      './media-controls': {
        ...mediaControls,
        MediaControls: class {
          constructor() {
            failAt('media-controls');
          }
        },
      },
    },
  });
  return {
    start: () => startApp({ qualityIndicator, videoGrid: {}, toastContainer: {} }, 'fixture'),
    failure,
    released,
    parent,
    qualityIndicator,
    eventsAfterFailure() {
      window.dispatchEvent(new Event('error'));
      window.dispatchEvent(new Event('unhandledrejection'));
      return telemetryEvents;
    },
  };
}

function assertRestored(f) {
  assert.deepEqual(f.parent.children, [f.qualityIndicator]);
  assert.equal(f.eventsAfterFailure(), 0, 'retired global listeners must never record again');
}

test('a failed screen constructor releases prior resources and global listeners', async () => {
  const f = await fixture('chrome');
  assert.throws(f.start, (error) => error === f.failure);
  assert.deepEqual(f.released, ['toasts', 'video-layout', 'telemetry']);
  assertRestored(f);
});

test('DOM adoption failure already owns its button and restores the static quality indicator', async () => {
  const f = await fixture('adopt-quality');
  assert.throws(f.start, (error) => error === f.failure);
  assert.deepEqual(f.released, ['video-layout', 'telemetry']);
  assertRestored(f);
});

test('later composition failure preserves the established dependency teardown order', async () => {
  const f = await fixture('media-controls');
  assert.throws(f.start, (error) => error === f.failure);
  assert.deepEqual(f.released, [
    'chrome',
    'invitations',
    'auth-screen',
    'home',
    'toasts',
    'video-layout',
    'auth',
    'telemetry',
  ]);
  assertRestored(f);
});

test('cleanup failure preserves the constructor cause and still attempts every prior resource', async () => {
  const cleanupFailure = new Error('fixture cleanup failed');
  const f = await fixture('media-controls', cleanupFailure);
  assert.throws(
    f.start,
    (error) => error.cause === f.failure && error.cleanupError === cleanupFailure,
  );
  assert.deepEqual(f.released, [
    'chrome',
    'invitations',
    'auth-screen',
    'home',
    'toasts',
    'video-layout',
    'auth',
    'telemetry',
  ]);
  assertRestored(f);
});
