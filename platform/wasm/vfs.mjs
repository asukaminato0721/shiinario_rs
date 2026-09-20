// File objects remain lazy. Only requested byte ranges enter the WASM heap.
export function normalize(path) {
  const parts = path.replaceAll('\\', '/').split('/').filter(p => p && p !== '.');
  if (path.startsWith('/') || parts.some(p => p === '..' || p.includes(':') || p.includes('\0'))) {
    throw new Error('Invalid path');
  }
  return parts.join('/');
}

export class DirectoryFs {
  constructor(entries, readBlob, persist = () => {}, saved = []) {
    this.nodes = new Map([['', null]]);
    this.overlay = new Map();
    this.readBlob = readBlob;
    this.persist = persist;
    for (const [rawPath, file] of entries) {
      const path = normalize(rawPath);
      if (!path || this.nodes.has(path)) throw new Error(`Duplicate path: ${path}`);
      const parts = path.split('/');
      for (let i = 1; i < parts.length; i++) {
        const parent = parts.slice(0, i).join('/');
        if (this.nodes.has(parent) && this.nodes.get(parent) !== null) throw new Error(`Not a directory: ${parent}`);
        this.nodes.set(parent, null);
      }
      this.nodes.set(path, file);
    }
    for (const [path, data] of saved) {
      const key = normalize(path);
      this.overlay.set(key, data === null ? null : new Uint8Array(data));
    }
  }
  node(path) {
    const key = normalize(path);
    if (this.overlay.has(key)) return this.overlay.get(key);
    if (!this.nodes.has(key)) throw new Error(`NotFound: ${key}`);
    return this.nodes.get(key);
  }
  stat(path) {
    const node = this.node(path);
    return [node === null, node === null ? 0 : (node.size ?? node.length)];
  }
  list(path) {
    const key = normalize(path);
    if (this.node(key) !== null) throw new Error(`Not a directory: ${key}`);
    const prefix = key ? `${key}/` : '';
    return [...new Set([...this.nodes.keys(), ...this.overlay.keys()])]
      .filter(p => p.startsWith(prefix) && p !== key && !p.slice(prefix.length).includes('/'))
      .map(p => p.slice(prefix.length)).sort();
  }
  read(path, offset, length) {
    if (!Number.isSafeInteger(offset) || offset < 0 || !Number.isSafeInteger(length) || length < 0) throw new Error('Invalid range');
    const node = this.node(path);
    if (node === null) throw new Error('Cannot read a directory');
    return node instanceof Uint8Array ? node.slice(offset, offset + length) : this.readBlob(node.slice(offset, offset + length));
  }
  commit(path, value) {
    const key = normalize(path);
    const parent = key.split('/').slice(0, -1).join('/');
    if (!key || this.node(parent) !== null) throw new Error('NotFound: parent directory');
    const next = new Map(this.overlay);
    next.set(key, value);
    let size = 0;
    for (const data of next.values()) size += data?.length ?? 0;
    if (size > 64 * 1024 * 1024) throw new Error('Save data exceeds 64 MiB');
    this.persist([...next].map(([name, data]) => [name, data === null ? null : Array.from(data)]));
    this.overlay = next;
  }
  write(path, data) {
    const key = normalize(path);
    if (data.length > 16 * 1024 * 1024) throw new Error('File write exceeds 16 MiB');
    if ((this.overlay.has(key) || this.nodes.has(key)) && this.node(key) === null) throw new Error('Cannot write a directory');
    this.commit(key, new Uint8Array(data));
  }
  mkdir(path) {
    const key = normalize(path);
    if (this.nodes.has(key) || this.overlay.has(key)) throw new Error(`AlreadyExists: ${key}`);
    this.commit(key, null);
  }
  install(target = globalThis) {
    target.shiinarioStat = p => JSON.stringify(this.stat(p));
    target.shiinarioList = p => JSON.stringify(this.list(p));
    target.shiinarioRead = (p, start, size) => this.read(p, start, size);
    target.shiinarioWrite = (p, bytes) => this.write(p, bytes);
    target.shiinarioMkdir = p => this.mkdir(p);
  }
}

// Two independent snapshots keep the previous save intact if a write is interrupted.
// Sync access handles are held by one worker, preventing concurrent writers.
export async function openSaveStore(gameId) {
  const root = await navigator.storage.getDirectory();
  const directory = await root.getDirectoryHandle(`shiinario-${gameId}`, { create: true });
  const handles = [];
  try {
    for (const name of ['a.json', 'b.json']) {
      const file = await directory.getFileHandle(name, { create: true });
      handles.push(await file.createSyncAccessHandle());
    }
    const snapshots = handles.map(handle => {
      if (handle.getSize() === 0) return null;
      if (handle.getSize() > 256 * 1024 * 1024) return null;
      const buffer = new Uint8Array(handle.getSize());
      handle.read(buffer, { at: 0 });
      try {
        const snapshot = JSON.parse(new TextDecoder().decode(buffer));
        if (!Number.isSafeInteger(snapshot.revision) || !Array.isArray(snapshot.files)) return null;
        return snapshot;
      } catch { return null; }
    });
    if (snapshots.every(s => !s) && handles.some(h => h.getSize() !== 0)) throw new Error('Browser save snapshots are damaged; refusing to overwrite them');
    let active = (snapshots[1]?.revision ?? -1) > (snapshots[0]?.revision ?? -1) ? 1 : 0;
    let revision = snapshots[active]?.revision ?? 0;
    return {
      files: snapshots[active]?.files ?? [],
      persist(files) {
        const buffer = new TextEncoder().encode(JSON.stringify({ revision: revision + 1, files }));
        if (buffer.length > 256 * 1024 * 1024) throw new Error('Save snapshot exceeds 256 MiB');
        const next = 1 - active;
        const handle = handles[next];
        // Invalidate the inactive snapshot before writing. The active one survives.
        handle.truncate(0);
        if (handle.write(buffer, { at: 0 }) !== buffer.length) throw new Error('Incomplete browser save write');
        handle.truncate(buffer.length);
        handle.flush();
        active = next;
        revision++;
      },
      close() { for (const handle of handles) handle.close(); },
    };
  } catch (error) {
    for (const handle of handles) handle.close();
    throw error;
  }
}
