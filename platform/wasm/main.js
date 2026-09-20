const $ = id => document.getElementById(id);
const canvas = $('screen');
const context = canvas.getContext('2d', { alpha: false });
let worker, audio, audioStart, audioNext, clockTimer, closing = false;
let audioSequence = 0;
const sources = new Set();
const status = message => { $('status').textContent = message; };
function post(data) { worker?.postMessage(data); }
function clearAudio() { for (const source of sources) { source.stop(); source.disconnect(); } sources.clear(); }
async function cleanup(message) {
  clearInterval(clockTimer);
  clearAudio();
  worker?.terminate(); worker = null;
  await audio?.close(); audio = null;
  $('start').disabled = false; $('stop').disabled = true;
  closing = false;
  status(message);
}
$('start').onclick = async () => {
  try {
    if (worker) return;
    if (!isSecureContext || !navigator.storage?.getDirectory) throw new Error('This player requires HTTPS or localhost and browser local-file storage support.');
    const files = Array.from($('game').files);
    const font = $('font').files[0];
    if (!files.length || !font) throw new Error('Select a game folder and a Japanese font first.');
    if (font.size > 32 * 1024 * 1024) throw new Error('Font exceeds 32 MiB.');
    $('start').disabled = true;
    audio = new AudioContext();
    await audio.resume();
    audioStart = audio.currentTime; audioNext = audioStart; audioSequence = 0;
    worker = new Worker('./worker.js', { type: 'module' });
    worker.onerror = event => cleanup(`Player error: ${event.message}`);
    worker.onmessage = ({ data }) => {
      switch (data.type) {
        case 'ready': status('Running. Saves are stored locally in this browser.'); $('stop').disabled = false; canvas.focus(); break;
        case 'frame': context.putImageData(new ImageData(new Uint8ClampedArray(data.frame), 800, 600), 0, 0); break;
        case 'title': document.title = data.title || 'Shiina Rio'; break;
        case 'fullscreen': status('Use the Fullscreen button to change browser display mode.'); break;
        case 'audio': {
          if (document.hidden || data.sequence < audioSequence) break;
          const frames = data.samples.length / 2;
          const buffer = audio.createBuffer(2, frames, audio.sampleRate);
          for (let channel = 0; channel < 2; channel++) {
            const out = buffer.getChannelData(channel);
            for (let i = 0; i < frames; i++) out[i] = data.samples[i * 2 + channel];
          }
          const source = audio.createBufferSource(); source.buffer = buffer; source.connect(audio.destination);
          sources.add(source); source.onended = () => { sources.delete(source); source.disconnect(); };
          audioNext = Math.max(audioNext, audio.currentTime + 0.005);
          source.start(audioNext); audioNext += frames / audio.sampleRate;
          break;
        }
        case 'error': cleanup(`Stopped: ${data.message}`); break;
        case 'stopped': cleanup('Stopped. Saves are stored locally.'); break;
      }
    };
    const entries = files.map(file => [file.webkitRelativePath.split('/').slice(1).join('/'), file]);
    post({ type: 'start', files: entries, font: await font.arrayBuffer(), rate: audio.sampleRate });
    clockTimer = setInterval(() => { if (audio && !document.hidden) post({ type: 'clock', time: audio.currentTime - audioStart }); }, 20);
    status('Opening game…');
  } catch (error) { await cleanup(String(error)); }
};
$('stop').onclick = () => { if (!closing) { closing = true; post({ type: 'stop' }); status('Stopping…'); } };
$('fullscreen').onclick = () => { (document.fullscreenElement ? document.exitFullscreen() : canvas.requestFullscreen()).catch(error => status(String(error))); };
function pointer(event, pressed, moving = false) {
  const rect = canvas.getBoundingClientRect();
  const scale = Math.min(rect.width / 800, rect.height / 600);
  const x = Math.floor((event.clientX - rect.left - (rect.width - 800 * scale) / 2) / scale);
  const y = Math.floor((event.clientY - rect.top - (rect.height - 600 * scale) / 2) / scale);
  post({ type: 'pointer', x, y, button: moving ? 0 : event.button === 2 ? 2 : event.button === 1 ? 3 : 1, pressed });
  event.preventDefault();
}
canvas.onpointerdown = event => { canvas.focus(); canvas.setPointerCapture(event.pointerId); pointer(event, true); };
canvas.onpointerup = event => pointer(event, false);
canvas.onpointercancel = event => pointer(event, false);
canvas.onpointermove = event => pointer(event, false, true);
canvas.oncontextmenu = event => event.preventDefault();
const keys = {
  Escape: [1, 27], Tab: [15, 9], Enter: [28, 13], NumpadEnter: [156, 13], Space: [57, 32],
  ControlLeft: [29, 162], ControlRight: [157, 163], ShiftLeft: [42, 160], ShiftRight: [54, 161],
  AltLeft: [56, 164], AltRight: [184, 165], Backspace: [14, 8],
  ArrowUp: [200, 38], ArrowDown: [208, 40], ArrowLeft: [203, 37], ArrowRight: [205, 39],
  Home: [199, 36], End: [207, 35], PageUp: [201, 33], PageDown: [209, 34], Insert: [210, 45], Delete: [211, 46],
};
const letterScans = [30,48,46,32,18,33,34,35,23,36,37,38,50,49,24,25,16,19,31,20,22,47,17,45,21,44];
for (let i = 0; i < 26; i++) keys[`Key${String.fromCharCode(65+i)}`] = [letterScans[i], 65+i];
for (let i = 0; i < 10; i++) keys[`Digit${i}`] = [i ? i+1 : 11, 48+i];
for (let i = 1; i <= 12; i++) keys[`F${i}`] = [i <= 10 ? 58+i : 76+i, 111+i];
for (const type of ['keydown', 'keyup']) canvas.addEventListener(type, event => {
  const key = keys[event.code];
  if (!key || event.repeat) return;
  post({ type: 'key', scan: key[0], vk: key[1], pressed: type === 'keydown', text: event.key.length === 1 });
  event.preventDefault();
});
window.addEventListener('blur', () => post({ type: 'focus', focused: false }));
window.addEventListener('focus', () => post({ type: 'focus', focused: true }));
document.addEventListener('visibilitychange', async () => {
  if (!audio || !worker) return;
  if (document.hidden) { clearAudio(); await audio.suspend(); }
  else { audioSequence++; await audio.resume(); audioStart = audio.currentTime; audioNext = audioStart; }
  post({ type: 'visibility', visible: !document.hidden });
});
