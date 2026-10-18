import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { runInNewContext } from 'node:vm';
import tracing from '../e2e/signaling-reconnect.cjs';
import menus from '../e2e/room-menu.cjs';

const source = await readFile(new URL('../e2e/media-continuity.cjs', import.meta.url), 'utf8');
const start = source.indexOf('function installReceiveRecoveryObservation(');
const end = source.indexOf('\n/** Native decode/playback counters', start);
assert.ok(start >= 0 && end > start);
const installSource = source.slice(start, end);

for (const initiallyOpen of [false, true]) {
  test(`Call health uses the visible More control when initially ${initiallyOpen ? 'open' : 'closed'}`, async () => {
    let open = initiallyOpen;
    let toggles = 0;
    let clicks = 0;
    const dialog = {};
    const page = {
      locator(selector) {
        if (selector === '#room-more-btn')
          return {
            isVisible: () => true,
            getAttribute: () => String(open),
            click() {
              toggles++;
              open = !open;
            },
          };
        assert.equal(selector, '#call-health-btn');
        return {
          click() {
            assert.equal(open, true, 'the real control cannot be clicked inside hidden More');
            clicks++;
          },
        };
      },
      getByRole(role, options) {
        assert.equal(role, 'dialog');
        assert.equal(options.name, 'Call health');
        assert.equal(options.exact, true);
        return dialog;
      },
    };
    const helperStart = source.indexOf('async function openCallHealth(');
    const helperEnd = source.indexOf('\nasync function receiverReplacement(', helperStart);
    assert.ok(helperStart >= 0 && helperEnd > helperStart);
    const openHealth = runInNewContext(`${source.slice(helperStart, helperEnd)}; openCallHealth`, {
      openRoomMenu: menus.openRoomMenu,
    });
    assert.equal(await openHealth(page), dialog);
    assert.equal(toggles, initiallyOpen ? 0 : 1);
    assert.equal(clicks, 1);
  });
}

function fixture(reconnectFirst = true) {
  const nativeCalls = [];
  class Peer {
    connectionState = 'connected';
    senders = [];
    receivers = [];
    getSenders() {
      return this.senders;
    }
    getReceivers() {
      return this.receivers;
    }
    setRemoteDescription(value) {
      nativeCalls.push({ peer: this, value });
      return Promise.resolve();
    }
  }
  class Socket {
    static OPEN = 1;
    readyState = 1;
    constructor(url) {
      this.url = url;
    }
    send() {
      return 'sent';
    }
    close() {}
    addEventListener() {}
  }
  const track = (kind) => ({
    id: 'private-track-identity',
    kind,
    readyState: 'live',
    enabled: true,
    muted: false,
  });
  const sender = new Peer();
  sender.senders = ['audio', 'video', 'video'].map((kind) => ({ track: track(kind) }));
  const receiver = new Peer();
  receiver.receivers = [{ track: track('video') }];
  const window = {
    RTCPeerConnection: Peer,
    WebSocket: Socket,
    __communityPeers: [sender, receiver],
    __mediaProduct: { captures: 2, screenCaptures: 1 },
    location: { protocol: 'http:', host: '127.0.0.1' },
  };
  const globals = {
    window,
    RTCPeerConnection: Peer,
    WebSocket: Socket,
    URL,
    performance: { now: () => 1000 },
  };
  const reconnect = `(${tracing.installSignalingReconnectObservation.toString()})();`;
  runInNewContext(
    `${reconnectFirst ? reconnect : ''}${installSource}; installReceiveRecoveryObservation();${reconnectFirst ? '' : reconnect}`,
    globals,
  );
  return { window, sender, receiver, Peer, nativeCalls };
}

test('one receive-only offer can hang while sending answers and subsequent native work stay untouched', async () => {
  const f = fixture();
  const api = f.window.__receiveRecoveryObservation;
  assert.equal(api.baseline().sendingTracks, 3);
  api.arm();
  await f.sender.setRemoteDescription({ type: 'answer', sdp: 'private-sdp' });
  await f.sender.setRemoteDescription({ type: 'offer', sdp: 'private-sdp' });
  let completed = false;
  f.receiver.setRemoteDescription({ type: 'offer', sdp: 'private-sdp' }).then(() => {
    completed = true;
  });
  await Promise.resolve();
  assert.equal(completed, false);
  assert.equal(api.snapshot().heldCount, 1);
  assert.equal(f.nativeCalls.length, 2);
  await f.receiver.setRemoteDescription({ type: 'offer' });
  assert.equal(f.nativeCalls.length, 3);
  assert.throws(() => api.arm(), /may be armed once/);
  const foreign = new f.Peer();
  await foreign.setRemoteDescription({ type: 'offer' });
  assert.equal(f.nativeCalls.length, 4);
  assert.doesNotMatch(JSON.stringify(api.snapshot()), /private|sdp|track|receiverId/i);
});

for (const reconnectFirst of [true, false]) {
  test(`fixed join/leave counters preserve the existing signaling observer when installed ${reconnectFirst ? 'after' : 'before'}`, () => {
    const f = fixture(reconnectFirst);
    f.window.__receiveRecoveryObservation.baseline();
    const socket = new f.window.WebSocket('ws://127.0.0.1/ws');
    assert.equal(
      socket.send(JSON.stringify({ type: 'joinRoom', token: 'private-secret' })),
      'sent',
    );
    assert.equal(f.window.__communitySignalingReconnect.snapshot().counters.sentJoinRoom, 1);
    assert.equal(f.window.__receiveRecoveryObservation.snapshot().membershipUnchanged, false);
    assert.doesNotMatch(
      JSON.stringify(f.window.__receiveRecoveryObservation.snapshot()),
      /private-secret|ws:/,
    );
  });
}

test('identity proof rejects changed sending tracks, capture, membership or a receiver that was not replaced', () => {
  const f = fixture();
  const api = f.window.__receiveRecoveryObservation;
  api.baseline();
  assert.equal(api.snapshot().senderPreserved, true);
  assert.equal(api.snapshot().receiverReplaced, false);
  const fresh = new f.Peer();
  fresh.receivers = f.receiver.receivers;
  f.window.__communityPeers.push(fresh);
  f.receiver.connectionState = 'closed';
  assert.equal(api.snapshot().receiverReplaced, true);
  f.sender.senders[0].track = { ...f.sender.senders[0].track };
  assert.equal(
    api.snapshot().senderPreserved,
    false,
    'an equivalent new track is not the original track',
  );
  f.window.__mediaProduct.captures++;
  assert.equal(api.snapshot().capturesUnchanged, false);
  f.Peer.prototype.setRemoteDescription = () => Promise.resolve();
  assert.throws(() => api.snapshot(), /replaced/);
});
