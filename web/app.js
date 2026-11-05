// lz-streamer · Oberfläche. Ohne Build-Schritt, ohne Abhängigkeiten.
'use strict';

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const fmt = new Intl.NumberFormat('de-DE', { maximumFractionDigits: 1 });
const fmt0 = new Intl.NumberFormat('de-DE', { maximumFractionDigits: 0 });

let cfg = null;          // aktuelle Konfiguration (Quelle der Wahrheit: Formular)
let last = null;         // letzter /api/state
let saveTimer = 0;
let celebrated = false;

// ---------- Darstellung ----------
function setTheme(t) {
  const r = document.documentElement;
  t === 'auto' ? delete r.dataset.theme : (r.dataset.theme = t);
  $$('.themes button').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.t === t)));
  try { localStorage.setItem('theme', t); } catch {}
}
$$('.themes button').forEach(b => b.addEventListener('click', () => setTheme(b.dataset.t)));
setTheme((() => { try { return localStorage.getItem('theme') || 'auto'; } catch { return 'auto'; } })());

function toast(text) {
  const t = $('#toast');
  t.textContent = text;
  t.dataset.show = '';
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => delete t.dataset.show, 3200);
}

// ---------- Konfiguration <-> Formular ----------
const get = (o, p) => p.split('.').reduce((a, k) => (a == null ? a : a[k]), o);
function set(o, p, v) {
  const ks = p.split('.'); const last = ks.pop();
  ks.reduce((a, k) => (a[k] ??= {}), o)[last] = v;
}

function fillForm() {
  for (const el of $$('[data-path]')) {
    if (el === document.activeElement) continue;
    const v = get(cfg, el.dataset.path);
    if (el.type === 'checkbox') el.checked = !!v;
    else el.value = v ?? '';
  }
  for (const seg of $$('[data-seg]')) {
    const v = get(cfg, seg.dataset.seg);
    $$('button', seg).forEach(b => b.setAttribute('aria-pressed', String(b.dataset.v === v)));
  }
  const v = cfg.encoder.video;
  const key = `${v.width}x${v.height}@${v.fps}`;
  const sel = $('#format');
  if (![...sel.options].some(o => o.value === key)) sel.add(new Option(`${v.width}×${v.height} ${v.fps} fps`, key));
  sel.value = key;
  if (document.activeElement !== $('#bitrate')) $('#bitrate').value = fmt.format(v.bitrate_kbps / 1000);
  applyVisibility();
}

function applyVisibility() {
  for (const el of $$('[data-show-mode]')) {
    const [p, want] = el.dataset.showMode.split('=');
    el.hidden = String(get(cfg, p)) !== want;
  }
  const role = cfg.role;
  $$('[data-role]').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.role === role)));
  $('#enc-form').hidden = role !== 'encoder';
  $('#dec-form').hidden = role !== 'decoder';
  $('#cl-title').firstChild.textContent = role === 'encoder' ? 'Bereit zum Senden ' : 'Bereit zum Empfangen ';
  $('#enc-device-field').hidden = cfg.encoder.input.kind === 'test';
}

function readField(el) {
  if (el.type === 'checkbox') return el.checked;
  if ('num' in el.dataset) { const n = parseInt(el.value, 10); return Number.isFinite(n) ? n : 0; }
  return el.value;
}

function changed(path, value) {
  set(cfg, path, value);
  applyVisibility();
  clearTimeout(saveTimer);
  saveTimer = setTimeout(save, 450);
}

async function save() {
  const r = await fetch('/api/config', { method: 'PUT', headers: { 'content-type': 'application/json' }, body: JSON.stringify(cfg) });
  if (!r.ok) { toast('Speichern fehlgeschlagen'); return; }
  cfg = await (await fetch('/api/config')).json(); // normalisiert (z. B. eingefügte srt://-Adresse)
  fillForm();
  if (last?.runtime?.running) toast('Gespeichert – Kanal startet mit den neuen Einstellungen neu');
}

for (const el of $$('[data-path]')) {
  const ev = el.tagName === 'SELECT' || el.type === 'checkbox' ? 'change' : 'input';
  el.addEventListener(ev, () => changed(el.dataset.path, readField(el)));
}
for (const seg of $$('[data-seg]')) {
  seg.addEventListener('click', e => {
    const b = e.target.closest('button'); if (!b) return;
    $$('button', seg).forEach(x => x.setAttribute('aria-pressed', String(x === b)));
    changed(seg.dataset.seg, b.dataset.v);
  });
}
$('#format').addEventListener('change', e => {
  const [, w, h, f] = e.target.value.match(/(\d+)x(\d+)@(\d+)/);
  Object.assign(cfg.encoder.video, { width: +w, height: +h, fps: +f });
  if (+w > 1920 && cfg.encoder.video.bitrate_kbps < 15000) cfg.encoder.video.bitrate_kbps = cfg.encoder.video.codec === 'h265' ? 15000 : 25000;
  fillForm();
  changed('encoder.video.width', +w);
});
$('#bitrate').addEventListener('input', e => {
  const n = parseFloat(e.target.value.replace(',', '.'));
  if (Number.isFinite(n) && n > 0) changed('encoder.video.bitrate_kbps', Math.round(n * 1000));
});
$$('[data-role]').forEach(b => b.addEventListener('click', () => {
  if (cfg.role === b.dataset.role) return;
  celebrated = false;
  changed('role', b.dataset.role);
}));

// ---------- Aktionen ----------
$('#btn-main').addEventListener('click', async () => {
  const running = last?.runtime?.running;
  if (!running) { clearTimeout(saveTimer); await save(); }
  await fetch(running ? '/api/stop' : '/api/start', { method: 'POST' });
  poll();
});
$('#btn-signal').addEventListener('click', async e => {
  const lost = e.currentTarget.getAttribute('aria-pressed') !== 'true';
  e.currentTarget.setAttribute('aria-pressed', String(lost));
  e.currentTarget.textContent = lost ? 'Eingang wieder freigeben' : 'Signalverlust testen';
  await fetch('/api/signal', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ lost }) });
});

// ---------- Anzeige ----------
function tile(label, value, unit = '', state = '') {
  return `<div class="tile" ${state ? `data-s="${state}"` : ''}><b>${value}${unit ? `<small>${unit}</small>` : ''}</b><span>${label}</span></div>`;
}
const dash = '–';
function dur(s) {
  if (!s) return dash;
  const h = Math.floor(s / 3600), m = Math.floor(s % 3600 / 60), x = Math.floor(s % 60);
  return h ? `${h}:${String(m).padStart(2, '0')}:${String(x).padStart(2, '0')}` : `${m}:${String(x).padStart(2, '0')}`;
}

function render(st) {
  last = st;
  const rt = st.runtime, s = rt.status, role = st.config.role;
  if (!cfg) { cfg = st.config; fillForm(); }

  // fehlende Bausteine
  $('#missing').hidden = !st.missing.length;
  $('#missing').textContent = st.missing.join(' ');

  // Geräte- und Encoderlisten
  fillSelect($('#enc-device'), st.devices.filter(d => d.direction === 'input'), cfg.encoder.input.device);
  fillSelect($('#dec-device'), st.devices.filter(d => d.direction === 'output'), cfg.decoder.device);
  const codec = cfg.encoder.video.codec;
  const encs = st.caps[codec] || [];
  const es = $('#enc-encoder');
  const want = ['auto', ...encs.map(e => e.factory)].join();
  if (es.dataset.list !== want) {
    es.innerHTML = '<option value="auto">Automatisch (Hardware zuerst)</option>' +
      encs.map(e => `<option value="${e.factory}">${e.label}</option>`).join('');
    es.dataset.list = want;
    es.value = cfg.encoder.video.encoder;
  }
  $('#detected').textContent = st.devices.some(d => d.direction === 'input')
    ? `Erkannt: ${st.devices.filter(d => d.direction === 'input').map(d => d.name).join(', ')} · bei „Automatisch“: ${st.detected_input}`
    : 'Keine DeckLink/UltraStudio gefunden – „Automatisch“ sendet das Testbild.';

  // Hinweise
  const hints = st.hints.map(h => `<li data-l="${h.level}">${esc(h.text)}</li>`).join('');
  $('#enc-hints').innerHTML = role === 'encoder' ? hints : '';
  $('#dec-hints').innerHTML = role === 'decoder' ? hints : '';

  // Bühne
  const running = rt.running, live = running && !!s;
  const btn = $('#btn-main');
  btn.textContent = running ? 'Stoppen' : (role === 'encoder' ? 'Senden starten' : 'Empfang starten');
  running ? (btn.dataset.live = '') : delete btn.dataset.live;
  $('#chip-live').hidden = !live;
  $('#chip-live').textContent = role === 'encoder' ? 'Live' : 'Empfang';
  $('#btn-signal').hidden = !(live && role === 'encoder');

  const sig = $('#chip-signal');
  sig.hidden = !live;
  if (live) {
    if (s.standby) { sig.dataset.s = 'warn'; sig.textContent = 'Kein Signal – Standbild läuft'; }
    else if (s.signal === 'ok') { sig.dataset.s = 'ok'; sig.textContent = s.input_format ? `Signal ${s.input_format}` : 'Signal ok'; }
    else { sig.dataset.s = 'warn'; sig.textContent = role === 'decoder' ? 'Warte auf Bild' : 'Signal unklar'; }
  }
  const peer = $('#chip-peer');
  peer.hidden = !live;
  if (live) {
    peer.dataset.s = s.srt.connected ? 'ok' : 'warn';
    peer.textContent = s.srt.connected ? (s.srt.peers > 1 ? `${s.srt.peers} Gegenstellen` : 'Gegenstelle verbunden') : 'Keine Gegenstelle';
  }
  const srt = role === 'encoder' ? st.config.encoder.srt : st.config.decoder.srt;
  const target = srt.mode === 'caller' ? `${srt.host || '127.0.0.1'}:${srt.port}` : `Port ${srt.port}`;
  $('#stage-title').textContent = !running ? 'Bereit' : !s ? rt.phase.replace(/^./, c => c.toUpperCase()) : (role === 'encoder' ? 'Sendet' : 'Empfängt');
  $('#stage-sub').textContent = !running
    ? (role === 'encoder' ? `${st.detected_input} → SRT ${srt.mode === 'caller' ? 'an ' + target : 'wartet auf ' + target}` : `SRT ${srt.mode === 'caller' ? 'von ' + target : 'wartet auf ' + target}`)
    : !s ? (rt.last_error ? `Letzter Fehler: ${rt.last_error}` : 'Kanal startet …')
    : role === 'encoder' ? `${s.input} · ${s.encoder_label}${s.rtmp ? ' · RTMP ' + s.rtmp : ''}` : `${s.input}${s.resolution ? ' · ' + s.resolution : ''}`;

  // Vorschau
  const img = $('#preview');
  if (live) {
    const u = `/api/preview.jpg?t=${Date.now()}`;
    const probe = new Image();
    probe.onload = () => { img.src = u; img.hidden = false; $('#preview-empty').hidden = true; };
    probe.src = u;
  } else { img.hidden = true; $('#preview-empty').hidden = false; }

  // Kennzahlen
  const q = s?.srt;
  const lostState = q && q.lost > 0 ? 'warn' : '';
  $('#tiles').innerHTML = role === 'encoder' ? [
    tile('Bitrate', s ? fmt.format(s.bitrate_kbps / 1000) : dash, s ? 'Mbit/s' : ''),
    tile('Laufzeit (RTT)', q?.rtt_ms != null ? fmt.format(q.rtt_ms) : dash, q?.rtt_ms != null ? 'ms' : ''),
    tile('Verloren / wiederholt', q ? `${fmt0.format(q.lost)} / ${fmt0.format(q.retransmitted)}` : dash, '', lostState),
    tile(s?.encoder_hw ? 'Encoder · Hardware' : 'Encoder', s ? esc((s.encoder || '').replace(/_/g, ' ')) : dash),
    tile('Bilder/s', s ? fmt.format(s.fps) : dash),
    tile('Sendezeit', dur(s?.uptime_s)),
    tile('Neustarts', fmt0.format(rt.restarts), '', rt.restarts ? 'warn' : ''),
  ].join('') : [
    tile('Bilder/s', s ? fmt.format(s.fps) : dash),
    tile('Format', s?.resolution ? esc(s.resolution) : dash),
    tile('Bitrate', s ? fmt.format(s.bitrate_kbps / 1000) : dash, s ? 'Mbit/s' : ''),
    tile('Laufzeit (RTT)', q?.rtt_ms != null ? fmt.format(q.rtt_ms) : dash, q?.rtt_ms != null ? 'ms' : ''),
    tile('Verloren', q ? fmt0.format(q.lost) : dash, '', lostState),
    tile('Empfangszeit', dur(s?.uptime_s)),
    tile('Neustarts', fmt0.format(rt.restarts), '', rt.restarts ? 'warn' : ''),
  ].join('');

  renderChecklist(st);

  // Ereignisse (neueste oben)
  $('#ev-restarts').textContent = rt.restarts ? `${rt.restarts} Neustart${rt.restarts > 1 ? 's' : ''}` : '';
  $('#events').innerHTML = rt.events.slice().reverse().map(e =>
    `<li data-l="${e.level}"><time>${new Date(e.at_ms).toLocaleTimeString('de-DE')}</time><span>${esc(e.text)}</span></li>`).join('')
    || '<li><time></time><span>Noch nichts passiert.</span></li>';
}

function renderChecklist(st) {
  const rt = st.runtime, s = rt.status, role = st.config.role;
  const srt = role === 'encoder' ? st.config.encoder.srt : st.config.decoder.srt;
  const items = role === 'encoder' ? [
    ['Eingang gewählt', true],
    ['Ziel festgelegt', srt.mode === 'listener' || !!srt.host.trim()],
    ['Kanal läuft', !!s],
    ['Gegenstelle verbunden', !!s?.srt.connected],
  ] : [
    ['Quelle festgelegt', srt.mode === 'listener' || !!srt.host.trim()],
    ['Empfang läuft', !!s],
    ['Bild kommt an', !!s && s.fps > 0],
  ];
  const done = items.filter(i => i[1]).length;
  $('#cl-count').textContent = `${done} von ${items.length}`;
  $('#cl-progress').style.setProperty('--p', done / items.length);
  done === items.length ? ($('#cl-progress').dataset.done = '') : delete $('#cl-progress').dataset.done;
  const html = items.map(([t, d]) => `<li ${d ? 'data-done' : ''}>${t}</li>`).join('');
  if ($('#cl-list').dataset.html !== html) { $('#cl-list').innerHTML = html; $('#cl-list').dataset.html = html; }
  if (done === items.length && !celebrated) { celebrated = true; toast(role === 'encoder' ? 'Stream steht.' : 'Bild kommt an.'); }
  if (!rt.running) celebrated = false;
}

function fillSelect(sel, devices, current) {
  const key = devices.map(d => d.device + d.name).join('|');
  if (sel.dataset.key === key) return;
  sel.dataset.key = key;
  sel.innerHTML = devices.length
    ? devices.map(d => `<option value="${d.device}">${esc(d.name)} (Nr. ${d.device})</option>`).join('')
    : '<option value="0">Kein Gerät gefunden (Nr. 0)</option>';
  sel.value = String(current);
}

function esc(s) { return String(s).replace(/[&<>"]/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' })[c]); }

async function poll() {
  try {
    const r = await fetch('/api/state');
    render(await r.json());
  } catch {
    $('#stage-title').textContent = 'Keine Verbindung';
    $('#stage-sub').textContent = 'Der lz-streamer-Dienst antwortet nicht. Läuft er noch?';
  }
}
poll();
setInterval(poll, 1000);
