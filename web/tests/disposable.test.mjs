import assert from 'node:assert/strict';
import test from 'node:test';
import { createScope, mount } from '../src/disposable.ts';

test('retiring a scope aborts event listeners before resource cleanup and runs once', () => {
  const scope = createScope();
  const target = new EventTarget();
  const events = [];
  target.addEventListener('change', () => events.push('event'), { signal: scope.signal });
  scope.onDispose(() => {
    target.dispatchEvent(new Event('change'));
    events.push('first');
  });
  scope.onDispose(() => events.push('second'));
  target.dispatchEvent(new Event('change'));
  scope.dispose();
  scope.dispose();
  assert.deepEqual(events, ['event', 'second', 'first']);
  scope.onDispose(() => events.push('late'));
  assert.equal(events.at(-1), 'late');
});

test('a failed cleanup still releases other resources and preserves its error', () => {
  const scope = createScope();
  const error = new Error('cleanup failed');
  let released = false;
  scope.onDispose(() => {
    released = true;
  });
  scope.onDispose(() => {
    throw error;
  });
  assert.throws(
    () => scope.dispose(),
    (actual) => actual === error,
  );
  assert.equal(released, true);
  assert.equal(scope.signal.aborted, true);
  scope.dispose();
});

test('mount owns its element and cleans listeners even when rendering fails', () => {
  const children = ['sibling'];
  const parent = { append: (node) => children.push(node) };
  const node = {
    remove() {
      children.splice(children.indexOf(node), 1);
    },
  };
  const scope = mount(parent, () => node);
  scope.dispose();
  assert.deepEqual(children, ['sibling']);
  const failure = new Error('render failed');
  let signal;
  assert.throws(
    () =>
      mount(parent, (owner) => {
        signal = owner.signal;
        throw failure;
      }),
    (error) => error === failure,
  );
  assert.equal(signal.aborted, true);
  assert.deepEqual(children, ['sibling']);
});
