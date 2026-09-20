import init, { BrowserPlayer } from './pkg/shiinario_runtime.js';
import { DirectoryFs, openSaveStore } from './vfs.mjs';

let player, saves, timer, started, pausedAt, paused = 0;
let visible = true;
let audioRate = 48000;
let audioFrames = 0;
let audioClock = 0;
let sequence = 0;
let stopping = false;
globalThis.shiinarioNow = () => performance.now();
globalThis.shiinarioTitle = title => postMessage({ type: 'title', title });
globalThis.shiinarioFullscreen = enabled => postMessage({ type: 'fullscreen', enabled });

function stop(error) {
  if (stopping) return;
  stopping = true;
  clearTimeout(timer);
  player?.free();
  player = null;
  saves?.close();
  saves = null;
  postMessage({ type: error ? 'error' : 'stopped', message: String(error ?? '') });
}
function tick() {
  try {
    if (!visible || !player) return;
    const elapsed = performance.now() - started - paused;
    const running = player.step(Math.floor(elapsed));
    const frame = player.frame();
    if (frame.length) postMessage({ type: 'frame', frame }, [frame.buffer]);
    // Keep at most 80 ms queued, bounded independently of interpreter speed.
    const wanted = Math.floor((audioClock + 0.08) * audioRate);
    const count = Math.min(4096, Math.max(0, wanted - audioFrames));
    if (count) {
      const samples = player.audio(count, audioRate);
      postMessage({ type: 'audio', samples, sequence }, [samples.buffer]);
      audioFrames += count;
    }
    if (!running) { stop(); return; }
    timer = setTimeout(tick, 1);
  } catch (error) { stop(error); }
}
onmessage = async ({ data }) => {
  try {
    switch (data.type) {
      case 'start': {
        const manifest = data.files.map(([path, file]) => [path, file.size, file.lastModified]).sort((a, b) => a[0].localeCompare(b[0]));
        const hash = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(JSON.stringify(manifest)));
        const id = Array.from(new Uint8Array(hash), b => b.toString(16).padStart(2, '0')).join('');
        saves = await openSaveStore(id);
        const reader = new FileReaderSync();
        new DirectoryFs(data.files, blob => new Uint8Array(reader.readAsArrayBuffer(blob)), files => saves.persist(files), saves.files).install();
        await init();
        player = new BrowserPlayer(new Uint8Array(data.font));
        audioRate = data.rate;
        started = performance.now();
        postMessage({ type: 'ready' });
        tick();
        break;
      }
      case 'pointer': player?.pointer(data.x, data.y, data.button, data.pressed); break;
      case 'key': player?.key(data.scan, data.vk, data.pressed, data.text); break;
      case 'clock': audioClock = data.time; break;
      case 'focus': player?.focus(data.focused); break;
      case 'visibility':
        if (visible === data.visible) break;
        visible = data.visible;
        player?.focus(visible);
        if (!visible) { pausedAt = performance.now(); clearTimeout(timer); }
        else { paused += performance.now() - pausedAt; audioFrames = 0; audioClock = 0; sequence++; tick(); }
        break;
      case 'stop': stop(); break;
    }
  } catch (error) { stop(error); }
};
