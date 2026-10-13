import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import vm from 'node:vm';

// Replace module imports with fake browser services. Run the actual worker code.
const source = readFileSync(new URL('./worker.js', import.meta.url), 'utf8')
  .replace(/^import .*;\n/gm, '');

async function worker(delay, wakeDuringResume = false) {
  let now = 0;
  let steps = 0;
  let nextId = 0;
  const timers = new Map();
  const messages = [];
  const context = vm.createContext({
    performance: { now: () => now },
    setTimeout: (callback, ms) => {
      const id = ++nextId;
      timers.set(id, { callback, ms });
      return id;
    },
    clearTimeout: id => timers.delete(id),
    postMessage: message => messages.push(message),
    onmessage: null,
    TextEncoder,
    crypto: { subtle: { digest: async () => new Uint8Array([1]).buffer } },
    init: async () => {},
    openSaveStore: async () => ({ files: [], close() {} }),
    FileReaderSync: class {},
    DirectoryFs: class { install() {} },
    BrowserPlayer: class {
      resume() {
        steps++;
        if (wakeDuringResume) context.shiinarioWake();
        return true;
      }
      next_wake_delay_ms() { return delay; }
      frame() { return new Uint8Array(); }
      audio(count) { return new Float32Array(count * 2); }
      pointer() {}
      key() {}
      focus() {}
      free() {}
    },
  });
  vm.runInContext(source, context);
  const send = data => context.onmessage({ data });
  await send({ type: 'start', files: [], font: [], rate: 48000 });
  return {
    timers, messages, send,
    wake: () => context.shiinarioWake(),
    steps: () => steps,
    advance(ms) {
      now += ms;
      assert.equal(timers.size, 1);
      const [id, timer] = timers.entries().next().value;
      timers.delete(id);
      timer.callback();
    },
  };
}

test('uses the script delay and retains the audio refill deadline', async () => {
  for (const [delay, expected] of [[0, 1], [7, 7], [1000, 20]]) {
    const w = await worker(delay);
    assert.equal(w.timers.size, 1);
    assert.equal(w.timers.values().next().value.ms, expected);
  }
});

test('input wakes the worker without creating duplicate timers', async () => {
  const w = await worker(1000);
  for (const type of ['pointer', 'key', 'focus']) {
    await w.send({ type });
    assert.equal(w.timers.size, 1);
    assert.equal(w.timers.values().next().value.ms, 0);
  }
  w.advance(1);
  assert.equal(w.steps(), 2);
  assert.equal(w.timers.size, 1);
});

test('refills audio during a long script wait', async () => {
  const w = await worker(1000);
  const initial = w.messages.filter(m => m.type === 'audio').length;
  await w.send({ type: 'clock', time: 0.02 });
  w.advance(20);
  assert.equal(w.messages.filter(m => m.type === 'audio').length, initial + 1);
  assert.equal(w.timers.values().next().value.ms, 20);
});

test('executor wakes replace the timer and retain one scheduled tick', async () => {
  const w = await worker(1000);
  w.wake();
  w.wake();
  assert.equal(w.timers.size, 1);
  assert.equal(w.timers.values().next().value.ms, 0);
  w.advance(0);
  assert.equal(w.steps(), 2);
  assert.equal(w.timers.size, 1);
});

test('executor wakes during resume merge into the next deadline', async () => {
  const w = await worker(0, true);
  assert.equal(w.timers.size, 1);
  assert.equal(w.timers.values().next().value.ms, 1);
  w.advance(1);
  assert.equal(w.steps(), 2);
  assert.equal(w.timers.size, 1);
});

test('hidden and stopped workers do not schedule input wakeups', async () => {
  const w = await worker(1000);
  await w.send({ type: 'visibility', visible: false });
  await w.send({ type: 'key' });
  w.wake();
  assert.equal(w.timers.size, 0);
  await w.send({ type: 'visibility', visible: true });
  assert.equal(w.timers.size, 1);
  await w.send({ type: 'stop' });
  await w.send({ type: 'pointer' });
  w.wake();
  assert.equal(w.timers.size, 0);
});
