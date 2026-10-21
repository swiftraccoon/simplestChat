import assert from 'node:assert/strict';
import test from 'node:test';
import { roomScreenFixture } from './room-screen-fixture.mjs';
import { deferred, flush } from './ui-fixture.mjs';

test('a late join completion cannot reopen a room after leaving or controller disposal', async () => {
  for (const retire of ['leave', 'dispose']) {
    for (const outcome of ['resolve', 'reject']) {
      const f = await roomScreenFixture();
      const pending = deferred();
      await f.connect({ join: () => pending.promise });
      if (retire === 'leave') await f.controller.leaveCurrentRoom();
      else f.controller.dispose();
      f.elements.roomScreen.hidden = true;
      if (outcome === 'resolve') pending.resolve('joined');
      else pending.reject(new Error('Retired join'));
      await f.settled();
      assert.equal(f.elements.roomScreen.hidden, true, `${retire}/${outcome}`);
      assert.deepEqual(f.state.notices, []);
      assert.deepEqual(f.state.errors, []);
    }
  }
});

test('controller disposal removes static actions and suppresses pending camera feedback', async () => {
  for (const outcome of ['resolve', 'reject']) {
    const pending = deferred();
    let captures = 0;
    const f = await roomScreenFixture({
      room: {
        hasMedia: true,
        toggleVideo() {
          captures++;
          return pending.promise;
        },
      },
    });
    f.elements.camBtn.click();
    assert.equal(captures, 1, 'capture begins in the original gesture');
    f.controller.dispose();
    f.elements.camBtn.click();
    f.elements.joinBtn.click();
    if (outcome === 'resolve') pending.resolve(true);
    else pending.reject(new Error('Retired camera'));
    await f.settled();
    await flush();
    assert.equal(captures, 1);
    assert.equal(f.state.rooms.length, 0);
    assert.deepEqual(f.state.buttonUpdates, []);
    assert.deepEqual(f.state.toasts, []);
    assert.equal(
      f.elements.roomMoreRepair.children.length,
      0,
      'owned toolbar additions are removed',
    );
  }
});

test('leaving releases old tile observers and a repeated participant gets a fresh observer', async () => {
  const observers = [];
  class ResizeObserver {
    constructor(callback) {
      this.callback = callback;
      this.disconnected = false;
      observers.push(this);
    }
    observe(element) {
      this.element = element;
    }
    disconnect() {
      this.disconnected = true;
    }
  }
  const f = await roomScreenFixture({ room: {}, globals: { ResizeObserver } });
  f.controls.renderRemoteTrack('alice', 'Alice', { kind: 'video' }, 'video');
  const oldVideo = observers[0].element;
  assert.ok(oldVideo.srcObject);
  await f.controller.leaveCurrentRoom();
  assert.equal(observers[0].disconnected, true);
  assert.equal(oldVideo.srcObject, null);
  let staleSizeChanges = 0;
  f.store.room = f.makeRoom({ setRemoteVideoSizeCap: () => staleSizeChanges++ });
  f.controls.renderRemoteTrack('alice', 'Alice', { kind: 'video' }, 'video');
  assert.equal(observers.length, 2);
  assert.notEqual(observers[1].element, oldVideo);
  observers[0].callback([{ contentRect: { width: 640 } }]);
  assert.equal(staleSizeChanges, 0, 'a queued callback cannot resize a replacement membership');
  f.controller.dispose();
  assert.equal(observers[1].disconnected, true);
});

test('disposing an owned password prompt clears its secret without leaving a replacement lifecycle', async () => {
  const f = await roomScreenFixture();
  let leaves = 0;
  const owner = f.makeRoom({
    join: async () => {
      throw new f.RoomPasswordRequiredError();
    },
    leave: async () => leaves++,
  });
  f.store.room = owner;
  const joining = f.controller.joinRoomWithPassword(owner, 'private-room', 'Guest');
  await flush();
  const field = f.document.getElementById('join-room-password');
  field.value = 'retired-secret';
  f.controller.dispose();
  assert.equal(await joining, null);
  assert.equal(field.value, '');
  assert.equal(leaves, 0);
});

test('retired client callbacks cannot reopen screens or update a replacement controller lifecycle', async () => {
  for (const retiredBy of ['dispose', 'replacement']) {
    const f = await roomScreenFixture();
    await f.connect();
    const events = f.state.events;
    if (retiredBy === 'dispose') f.controller.dispose();
    else f.store.room = f.makeRoom();
    f.elements.lobbyScreen.hidden = true;
    f.elements.roomScreen.hidden = true;
    f.elements.roomTopic.textContent = 'Current topic';
    f.state.calls.length = 0;
    events.onLobbyWaiting('Late room', 'Late topic', 2, 1);
    events.onLobbyStatus(2, 1);
    events.onLobbyAdmitted();
    events.onAdmissionComplete();
    events.onParticipantsChanged(new Map());
    events.onTopicChanged('Retired topic', 'Retired name');
    events.onRecoveryState('failed', 'Retired recovery');
    events.onRoomClosed('Retired close');
    assert.equal(await events.onPasswordRequired(), null);
    await f.settled();
    assert.equal(f.elements.lobbyScreen.hidden, true, retiredBy);
    assert.equal(f.elements.roomScreen.hidden, true, retiredBy);
    assert.equal(f.elements.roomTopic.textContent, 'Current topic', retiredBy);
    assert.deepEqual(f.state.calls, [], retiredBy);
    assert.deepEqual(f.state.toasts, [], retiredBy);
    assert.deepEqual(f.state.notices, [], retiredBy);
  }
});
