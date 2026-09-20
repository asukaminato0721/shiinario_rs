import assert from 'node:assert/strict';
import test from 'node:test';
import { DirectoryFs, normalize } from './vfs.mjs';

test('normalizes game paths and rejects traversal and absolute paths', () => {
  assert.equal(normalize('./BG\\scene.s25'), 'BG/scene.s25');
  for (const path of ['../save', 'a/../save', '/save', 'C:\\save', 'bad\0name']) {
    assert.throws(() => normalize(path), /Invalid path/);
  }
});

test('indexes directories without reading files and reads only requested ranges', () => {
  const ranges = [];
  const file = {
    size: 1024 * 1024 * 1024,
    slice(start, end) { ranges.push([start, end]); return { start, end }; },
  };
  const fs = new DirectoryFs([['data/GAME.WAR', file]], ({ start, end }) => new Uint8Array(end - start));
  assert.deepEqual(fs.list('.'), ['data']);
  assert.deepEqual(fs.list('data'), ['GAME.WAR']);
  assert.deepEqual(fs.stat('data'), [true, 0]);
  assert.deepEqual(fs.stat('data/GAME.WAR'), [false, file.size]);
  assert.equal(ranges.length, 0);
  assert.equal(fs.read('data/GAME.WAR', 4096, 32).length, 32);
  assert.deepEqual(ranges, [[4096, 4128]]);
  assert.throws(() => fs.read('missing', 0, 1), /NotFound/);
  assert.throws(() => fs.read('data', 0, 1), /directory/);
  assert.throws(() => fs.read('data/GAME.WAR', -1, 1), /Invalid range/);
});

test('rejects duplicate paths and files used as parent directories', () => {
  assert.throws(() => new DirectoryFs([['a', {}], ['a', {}]]), /Duplicate path/);
  assert.throws(() => new DirectoryFs([['a', {}], ['a/b', {}]]), /Not a directory/);
});

test('persists original save bytes and restores files and directories', () => {
  let snapshot;
  const fs = new DirectoryFs([], undefined, files => { snapshot = structuredClone(files); });
  fs.mkdir('save');
  const bytes = new Uint8Array([0, 128, 255, 1]);
  fs.write('save/slot.dat', bytes);
  bytes.fill(42);
  assert.deepEqual(fs.read('save/slot.dat', 1, 2), new Uint8Array([128, 255]));
  const restored = new DirectoryFs([], undefined, () => {}, snapshot);
  assert.deepEqual(restored.list('save'), ['slot.dat']);
  assert.deepEqual(restored.read('save/slot.dat', 0, 8), new Uint8Array([0, 128, 255, 1]));
  assert.throws(() => restored.mkdir('save'), /AlreadyExists/);
  assert.throws(() => restored.write('save', bytes), /directory/);
  assert.throws(() => restored.write('missing/slot.dat', bytes), /NotFound/);
});

test('failed persistence keeps the previous save visible', () => {
  let fail = false;
  const fs = new DirectoryFs([], undefined, () => { if (fail) throw new Error('Disk full'); });
  fs.write('slot.dat', new Uint8Array([1, 2, 3]));
  fail = true;
  assert.throws(() => fs.write('slot.dat', new Uint8Array([9])), /Disk full/);
  assert.deepEqual(fs.read('slot.dat', 0, 10), new Uint8Array([1, 2, 3]));
  assert.throws(() => fs.mkdir('save'), /Disk full/);
  assert.throws(() => fs.stat('save'), /NotFound/);
});

test('installs the browser imports used by Rust', () => {
  const fs = new DirectoryFs([], undefined);
  const imports = {};
  fs.install(imports);
  imports.shiinarioMkdir('save');
  imports.shiinarioWrite('save/slot.dat', new Uint8Array([7]));
  assert.deepEqual(JSON.parse(imports.shiinarioStat('save/slot.dat')), [false, 1]);
  assert.deepEqual(JSON.parse(imports.shiinarioList('save')), ['slot.dat']);
  assert.deepEqual(imports.shiinarioRead('save/slot.dat', 0, 1), new Uint8Array([7]));
});
