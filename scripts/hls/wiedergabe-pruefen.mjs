// HLS-Testwiedergabe in Chrome (headless, Remote-Debugging) prüfen:
// spielt ab, Stufen/Tonspuren/Untertitel sichtbar, Ton- und Stufenwechsel wirken.
// Aufruf: node scripts/hls/wiedergabe-pruefen.mjs http://127.0.0.1:<Port>/hls-test
// Chrome-Pfad über CHROME, sonst der Standardpfad am Mac.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const url = process.argv[2] || 'http://127.0.0.1:8080/hls-test';
const chrome = process.env.CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
const port = 9300 + Math.floor(Math.random() * 500);
const profile = mkdtempSync(join(tmpdir(), 'lzs-chrome-'));
const proc = spawn(chrome, ['--headless=new', `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`,
  '--autoplay-policy=no-user-gesture-required', '--no-first-run', '--mute-audio', 'about:blank'], { stdio: 'ignore' });
const sleep = ms => new Promise(r => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 50; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      const t = list.find(x => x.type === 'page');
      if (t) return t.webSocketDebuggerUrl;
    } catch {}
    await sleep(200);
  }
  throw new Error('Chrome meldet sich nicht');
}

let ws, seq = 0;
const pending = new Map();
function send(method, params = {}) {
  const id = ++seq;
  ws.send(JSON.stringify({ id, method, params }));
  return new Promise((res, rej) => pending.set(id, { res, rej }));
}
async function evaluate(expr) {
  const r = await send('Runtime.evaluate', { expression: expr, returnByValue: true, awaitPromise: true });
  if (r.exceptionDetails) throw new Error(r.exceptionDetails.text);
  return r.result.value;
}

const checks = [];
const check = (name, ok, info) => { checks.push({ name, ok: !!ok, info }); };
try {
  ws = new WebSocket(await target());
  await new Promise(r => ws.addEventListener('open', r));
  ws.addEventListener('message', e => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) { pending.get(m.id).res(m.result || {}); pending.delete(m.id); }
  });
  await send('Page.navigate', { url });
  await sleep(12000);
  const s1 = await evaluate(`({ ...hlsTestState, t: document.getElementById('v').currentTime, w: document.getElementById('v').videoWidth })`);
  check('spielt ab', s1.playing && s1.t > 2, `Zeit ${s1.t?.toFixed(1)} s, ${s1.engine}`);
  check('Stufen in der Master-Playlist', s1.levels.length >= 2, s1.levels.join(', '));
  check('Tonspuren', s1.audio.length >= 2, s1.audio.join(', '));
  // feste Stufe (hls.js sortiert aufsteigend: 0 = kleinste) und zweite Tonspur wählen
  await evaluate(`(() => { const l = document.getElementById('level'); l.value = '0'; l.onchange({ target: l });
    const a = document.getElementById('audio'); a.value = '1'; a.onchange({ target: a }); })()`);
  await sleep(10000);
  const s2 = await evaluate(`({ ...hlsTestState, t: document.getElementById('v').currentTime, w: document.getElementById('v').videoWidth, h: document.getElementById('v').videoHeight,
    cues: [...document.getElementById('v').textTracks].map(t => ({ label: t.label, kind: t.kind, cues: t.cues ? t.cues.length : null, text: t.cues && t.cues.length ? t.cues[t.cues.length - 1].text : '' })) })`);
  check('Tonspur gewechselt', s2.audioTrack === 1, `Spur ${s2.audioTrack}`);
  check('Stufe gewechselt', s2.level === 0 && `${s2.w}x${s2.h}` === s1.levels[0], `${s2.w}×${s2.h}`);
  check('läuft weiter', s2.t > s1.t + 5, `Zeit ${s2.t.toFixed(1)} s`);
  check('Untertitel (CC1) als Textspur', s2.cues.some(c => c.cues > 0), JSON.stringify(s2.cues));
  check('kein Fehler', !s2.error, s2.error || '');
  if (process.argv.includes('--neustart')) {
    // Kanal neu starten: Playlists beginnen neu, Wiedergabe muss weiterlaufen
    const origin = new URL(url).origin;
    await fetch(`${origin}/api/stop`, { method: 'POST' });
    await sleep(1500);
    await fetch(`${origin}/api/start`, { method: 'POST' });
    await sleep(25000);
    const s3 = await evaluate(`({ ...hlsTestState, t: document.getElementById('v').currentTime, paused: document.getElementById('v').paused })`);
    check('läuft nach Neustart weiter', s3.t > s2.t + 10 && !s3.error, `Zeit ${s3.t.toFixed(1)} s${s3.error ? `, ${s3.error}` : ''}`);
  }
} catch (e) {
  check('Ablauf', false, String(e));
} finally {
  try { ws?.close(); } catch {}
  proc.kill();
  await sleep(500);
  rmSync(profile, { recursive: true, force: true });
}
for (const c of checks) console.log(`${c.ok ? 'ok    ' : 'FEHLER'} ${c.name}${c.info ? ` – ${c.info}` : ''}`);
process.exit(checks.every(c => c.ok) ? 0 : 1);
