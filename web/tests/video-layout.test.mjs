import assert from 'node:assert/strict';
import test from 'node:test';
import { loadTypeScript } from './native-fixtures.mjs';

const { calculateVideoLayout } = await loadTypeScript('src/video-layout.ts');
const landscape = 16 / 9;

function packedHeight(sizes, width, gap = 8) {
  let occupied = 0;
  let rows = 1;
  for (const size of sizes) {
    assert.ok(size.width <= width, 'no tile exceeds the stage width');
    if (occupied && occupied + gap + size.width > width) {
      rows++;
      occupied = size.width;
    } else occupied += (occupied ? gap : 0) + size.width;
  }
  return rows * sizes[0].height + (rows - 1) * gap;
}

test('four mixed media tiles have identical 16:9 cells in two useful rows', () => {
  const sizes = calculateVideoLayout(4, 900, 787);
  assert.ok(sizes[0].height > 240);
  assert.ok(sizes[0].height < 251, 'two useful rows win over three slightly larger rows');
  assert.ok(packedHeight(sizes, 900) <= 787);
  sizes.forEach((size) => {
    assert.deepEqual(size, sizes[0]);
    assert.ok(Math.abs(size.width / size.height - landscape) < 1e-10);
  });
});

test('phone layout fits four uniform cells without sacrificing readable height', () => {
  const sizes = calculateVideoLayout(4, 358, 340);
  assert.ok(packedHeight(sizes, 358) <= 340);
  assert.ok(
    sizes.every((size) => size.width === sizes[0].width && size.height === sizes[0].height),
  );
  assert.ok(sizes[0].height >= 96);
});

test('crowded rooms retain readable rows for scrolling instead of shrinking every tile', () => {
  const sizes = calculateVideoLayout(13, 358, 280);
  assert.equal(sizes[0].height, 96);
  assert.ok(sizes[0].width * 2 + 8 <= 358, 'crowded phone rows still fit two cameras');
  assert.ok(packedHeight(sizes, 358) > 280);
  const narrow = calculateVideoLayout(2, 120, 300);
  assert.ok(narrow.every((size) => size.width <= 120));
  assert.ok(packedHeight(narrow, 120) <= 300);
});

test('odd counts retain the same dimensions in every cell, including the last row', () => {
  for (const count of [1, 3, 5, 7, 13]) {
    for (const [width, height] of [
      [900, 700],
      [358, 340],
      [640, 220],
    ]) {
      const sizes = calculateVideoLayout(count, width, height);
      assert.equal(sizes.length, count);
      for (const size of sizes) {
        assert.deepEqual(size, sizes[0]);
        assert.ok(size.width <= width);
        assert.ok(Math.abs(size.width / size.height - landscape) < 1e-10);
      }
      assert.ok(sizes[0].height >= 96, 'crowded stages scroll instead of crushing tiles');
    }
  }
});

test('resizing the stage resizes all cells together without exceeding its width', () => {
  const desktop = calculateVideoLayout(5, 900, 700);
  const phone = calculateVideoLayout(5, 358, 340);
  assert.ok(desktop[0].width > phone[0].width);
  assert.ok(packedHeight(desktop, 900) <= 700);
  assert.ok(packedHeight(phone, 358) <= 340);
  assert.deepEqual(calculateVideoLayout(0, 600, 400), []);
  assert.deepEqual(calculateVideoLayout(Number.NaN, 600, 400), []);
  assert.deepEqual(calculateVideoLayout(1, 0, 400), [{ width: 0, height: 0 }]);
  assert.deepEqual(
    calculateVideoLayout(4, 600, 400, Number.NaN),
    calculateVideoLayout(4, 600, 400),
  );
});

test('layout disposal cancels its frame, disconnects observers and retires queued callbacks', async () => {
  const observers = [];
  const frames = new Map();
  const cancelled = [];
  const writes = [];
  let frameId = 0;
  class Observer {
    disconnected = 0;
    constructor(callback) {
      this.callback = callback;
      observers.push(this);
    }
    observe(target, options) {
      this.target = target;
      this.options = options;
    }
    disconnect() {
      this.disconnected++;
    }
  }
  class Element {
    classList = { contains: (name) => name === 'video-tile' };
    style = { setProperty: (...args) => writes.push(args) };
  }
  const grid = { children: [new Element()], clientWidth: 640, clientHeight: 360 };
  const { observeVideoLayout } = await loadTypeScript('src/video-layout.ts', {
    globals: {
      HTMLElement: Element,
      ResizeObserver: Observer,
      MutationObserver: Observer,
      requestAnimationFrame(callback) {
        const id = ++frameId;
        frames.set(id, callback);
        return id;
      },
      cancelAnimationFrame(id) {
        cancelled.push(id);
        frames.delete(id);
      },
      getComputedStyle: () => ({
        paddingLeft: '0',
        paddingRight: '0',
        paddingTop: '0',
        paddingBottom: '0',
        columnGap: '8',
      }),
    },
  });
  const dispose = observeVideoLayout(grid);
  assert.deepEqual(
    observers.map(({ target }) => target),
    [grid, grid],
  );
  assert.deepEqual(observers[1].options, { childList: true });
  observers.forEach(({ callback }) => callback());
  assert.equal(frames.size, 1, 'observer signals coalesce into the initial frame');
  const first = frames.get(1);
  frames.delete(1);
  first();
  assert.equal(writes.length, 2, 'a live frame sizes the actual tile');
  observers[0].callback();
  const retired = frames.get(2);
  dispose();
  assert.deepEqual(cancelled, [2]);
  assert.equal(frames.size, 0);
  assert.deepEqual(
    observers.map(({ disconnected }) => disconnected),
    [1, 1],
  );
  retired();
  observers.forEach(({ callback }) => callback());
  assert.equal(writes.length, 2, 'a delivered retired frame cannot write tile styles');
  assert.equal(frames.size, 0, 'retired observer delivery cannot schedule more frames');
  dispose();
  assert.deepEqual(cancelled, [2]);
});
