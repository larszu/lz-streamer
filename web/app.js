// lz-streamer · Oberfläche. Ohne Build-Schritt, ohne Abhängigkeiten.
'use strict';

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const fmt = new Intl.NumberFormat('de-DE', { maximumFractionDigits: 1 });
const fmt0 = new Intl.NumberFormat('de-DE', { maximumFractionDigits: 0 });

let cfg = null;          // aktuelle Konfiguration (Quelle der Wahrheit: Formular)
let last = null;         // letzter /api/state
let saveTimer = 0;

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
    el.hidden = !want.split('|').includes(String(get(cfg, p)));
  }
  const role = cfg.role;
  $$('[data-role]').forEach(b => b.setAttribute('aria-pressed', String(b.dataset.role === role)));
  $('#enc-form').hidden = role !== 'encoder';
  $('#dec-form').hidden = role !== 'decoder';
  $('#enc-device-field').hidden = cfg.encoder.input.kind === 'test' || cfg.encoder.input.kind === 'file';
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
  const target = srt.mode !== 'listener' ? `${srt.host || '127.0.0.1'}:${srt.port}` : `Port ${srt.port}`;
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


  // Ereignisse (neueste oben)
  $('#ev-restarts').textContent = rt.restarts ? `${rt.restarts} Neustart${rt.restarts > 1 ? 's' : ''}` : '';
  $('#events').innerHTML = rt.events.slice().reverse().map(e =>
    `<li data-l="${e.level}"><time>${new Date(e.at_ms).toLocaleTimeString('de-DE')}</time><span>${esc(e.text)}</span></li>`).join('')
    || '<li><time></time><span>Noch nichts passiert.</span></li>';
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
    if (r.status === 401) { showLogin(); return; }
    render(await r.json());
  } catch {
    $('#stage-title').textContent = 'Keine Verbindung';
    $('#stage-sub').textContent = 'Der lz-streamer-Dienst antwortet nicht. Läuft er noch?';
  }
}
poll();
setInterval(poll, 1000);

// ================================================================ Listen-Editoren
function rowEditor(host, items, fields, onChange, removeLabel) {
  host.innerHTML = '';
  items.forEach((item, i) => {
    const row = document.createElement('div');
    row.className = 'edit-row';
    for (const f of fields) {
      if (f.show && !f.show(item)) continue;
      const lab = document.createElement('label');
      lab.className = f.type === 'check' ? 'switch' : 'field';
      let el;
      if (f.type === 'select') {
        el = document.createElement('select');
        for (const [v, t] of f.options) el.add(new Option(t, v));
        el.value = String(f.get ? f.get(item) : item[f.key]);
      } else if (f.type === 'check') {
        el = document.createElement('input'); el.type = 'checkbox';
        el.checked = !!(f.get ? f.get(item) : item[f.key]);
      } else if (f.type === 'days') {
        el = document.createElement('div'); el.className = 'days';
        ['Mo', 'Di', 'Mi', 'Do', 'Fr', 'Sa', 'So'].forEach((d, k) => {
          const b = document.createElement('button'); b.type = 'button'; b.textContent = d;
          b.setAttribute('aria-pressed', String(item.days.includes(k + 1)));
          b.onclick = () => {
            item.days = item.days.includes(k + 1) ? item.days.filter(x => x !== k + 1) : [...item.days, k + 1].sort();
            b.setAttribute('aria-pressed', String(item.days.includes(k + 1)));
            onChange();
          };
          el.append(b);
        });
      } else {
        el = document.createElement('input');
        el.value = f.get ? f.get(item) : (item[f.key] ?? '');
        if (f.placeholder) el.placeholder = f.placeholder;
        if (f.type === 'password') el.type = 'password';
      }
      if (f.type !== 'days') {
        el.className = f.type === 'check' ? '' : 'input';
        const ev = f.type === 'select' || f.type === 'check' ? 'change' : 'input';
        el.addEventListener(ev, () => {
          const raw = f.type === 'check' ? el.checked : el.value;
          if (f.set) f.set(item, raw); else item[f.key] = f.num ? (parseFloat(String(raw).replace(',', '.')) || 0) : raw;
          if (f.rerender) rowEditor(host, items, fields, onChange, removeLabel);
          onChange();
        });
      }
      if (f.type === 'check') { lab.append(el, ' ' + f.label); }
      else { const sp = document.createElement('span'); sp.textContent = f.label; lab.append(sp, el); }
      row.append(lab);
    }
    const del = document.createElement('button');
    del.type = 'button'; del.className = 'btn btn--danger'; del.textContent = removeLabel;
    del.onclick = () => { items.splice(i, 1); rowEditor(host, items, fields, onChange, removeLabel); onChange(); };
    row.append(del);
    host.append(row);
  });
}

const trackFields = [
  { key: 'name', label: 'Name' },
  { key: 'language', label: 'Sprache', placeholder: 'deu' },
  { key: 'layout', label: 'Format', type: 'select', options: [['stereo', 'Stereo'], ['mono', 'Mono']] },
  { key: 'channels', label: 'Kanäle', get: t => t.channels.join(','), set: (t, v) => { t.channels = String(v).split(/[ ,;]+/).map(Number).filter(n => n > 0); } },
  { key: 'gain_db', label: 'Pegel (dB)', num: true },
  { key: 'bitrate_kbps', label: 'kbit/s', num: true },
  { key: 'mute', label: 'Stumm', type: 'check' },
  { key: 'lev', label: 'Lautheit regeln', type: 'check', rerender: true, get: t => t.leveler?.enabled, set: (t, v) => { t.leveler = { ...(t.leveler || {}), enabled: v }; } },
  { key: 'levt', label: 'Ziel (LUFS)', show: t => t.leveler?.enabled, get: t => t.leveler.target, set: (t, v) => { t.leveler.target = parseFloat(String(v).replace(',', '.')) || -23; } },
  { key: 'levp', label: 'Regelung', type: 'select', show: t => t.leveler?.enabled, options: [['soft', 'sanft'], ['mid', 'mittel'], ['hard', 'schnell']], get: t => t.leveler.preset, set: (t, v) => { t.leveler.preset = v; } },
];
const outputFields = [
  { key: 'enabled', label: 'An', type: 'check' },
  { key: 'name', label: 'Name' },
  { key: 'kind', label: 'Art', type: 'select', options: [['srt', 'SRT'], ['udp', 'UDP (TS)'], ['rtp', 'RTP (TS)']], rerender: true },
  { key: 'mode', label: 'SRT-Modus', type: 'select', options: [['caller', 'An Ziel senden'], ['listener', 'Auf Gegenstelle warten']], show: o => o.kind === 'srt', get: o => o.srt.mode, set: (o, v) => { o.srt.mode = v; } },
  { key: 'shost', label: 'Ziel', show: o => o.kind === 'srt', get: o => o.srt.host, set: (o, v) => { o.srt.host = v; } },
  { key: 'sport', label: 'Port', show: o => o.kind === 'srt', get: o => o.srt.port, set: (o, v) => { o.srt.port = parseInt(v, 10) || 0; } },
  { key: 'host', label: 'Ziel (auch Multicast)', show: o => o.kind !== 'srt' },
  { key: 'port', label: 'Port', num: true, show: o => o.kind !== 'srt' },
];
function renderEditors() {
  rowEditor($('#replacements'), cfg.encoder.captions.asr.replacements, [{ key: 'from', label: 'erkannt' }, { key: 'to', label: 'geschrieben' }],
    () => changed('encoder.captions.asr.replacements', cfg.encoder.captions.asr.replacements), 'Entfernen');
  rowEditor($('#tracks'), cfg.encoder.audio.tracks, trackFields, () => changed('encoder.audio.tracks', cfg.encoder.audio.tracks), 'Entfernen');
  rowEditor($('#outputs'), cfg.encoder.outputs, outputFields, () => changed('encoder.outputs', cfg.encoder.outputs), 'Entfernen');
}
$('#add-replacement').onclick = () => {
  cfg.encoder.captions.asr.replacements.push({ from: '', to: '' });
  renderEditors(); changed('encoder.captions.asr.replacements', cfg.encoder.captions.asr.replacements);
};
$('#add-track').onclick = () => {
  const n = cfg.encoder.audio.tracks.length;
  cfg.encoder.audio.tracks.push({ name: `Ton ${n + 1}`, language: 'und', layout: 'stereo', channels: [2 * n + 1, 2 * n + 2], gain_db: 0, mute: false, bitrate_kbps: 192, leveler: { enabled: false, target: -23, preset: 'mid', ceiling: -1, range_db: 15, freeze_below: -45 } });
  renderEditors(); changed('encoder.audio.tracks', cfg.encoder.audio.tracks);
};
$('#add-output').onclick = () => {
  cfg.encoder.outputs.push({ enabled: true, name: `Ausgang ${cfg.encoder.outputs.length + 2}`, kind: 'srt', srt: { mode: 'caller', host: '', port: 9001, latency_ms: 120, passphrase: '', streamid: '' }, host: '239.0.0.1', port: 5000, ttl: 16 });
  renderEditors(); changed('encoder.outputs', cfg.encoder.outputs);
};
const _fillForm = fillForm;
fillForm = function () { _fillForm(); if (!['#tracks', '#outputs', '#replacements'].some(id => $(id).contains(document.activeElement))) renderEditors(); };

// ================================================================ Ansichten
let view = 'betrieb';
$$('[data-view]').forEach(b => b.addEventListener('click', () => {
  view = b.dataset.view;
  $$('[data-view]').forEach(x => x.setAttribute('aria-pressed', String(x === b)));
  $('#view-betrieb').hidden = view !== 'betrieb';
  $('#view-verwaltung').hidden = view !== 'verwaltung';
  $('#role-seg').hidden = view !== 'betrieb';
  if (view === 'verwaltung') loadAdmin();
}));

async function api(method, url, body) {
  const r = await fetch(url, { method, headers: body !== undefined ? { 'content-type': 'application/json' } : {}, body: body !== undefined ? JSON.stringify(body) : undefined });
  if (r.status === 401) { showLogin(); throw new Error('login'); }
  const j = await r.json().catch(() => ({}));
  if (!r.ok) { toast(j.error || 'Fehlgeschlagen'); throw new Error(j.error); }
  return j;
}

// ---------------- Anmeldung
function showLogin() { const d = $('#login'); if (!d.open) d.showModal(); }
$('#login-form').addEventListener('submit', async e => {
  e.preventDefault();
  const r = await fetch('/api/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ name: $('#l-name').value, password: $('#l-pw').value }) });
  if (r.ok) { $('#login').close(); $('#l-err').textContent = ''; cfg = null; poll(); }
  else $('#l-err').textContent = 'Name oder Passwort falsch.';
});
$('#logout').onclick = async () => { await fetch('/api/logout', { method: 'POST' }); location.reload(); };

// ---------------- Verwaltung laden
let settings = null;
async function loadAdmin() {
  try {
    renderModels(await api('GET', '/api/models'));
    const [presets, s, recs, imgs, users, stats, me] = await Promise.all([
      api('GET', '/api/presets'), api('GET', '/api/settings'), api('GET', '/api/recordings'),
      api('GET', '/api/standby'), api('GET', '/api/users'), api('GET', '/api/stats?minutes=60'), api('GET', '/api/me')]);
    settings = s;
    $('#auto-delete').checked = !!s.delete_oldest_recordings;
    renderPresets(presets); renderSchedule(presets); renderAlarmFields(); renderRecordings(recs);
    renderStandby(imgs); renderUsers(users, me); renderChart(stats);
  } catch {}
}

function renderPresets(list) {
  $('#presets').innerHTML = list.map(p => `<li><span>${esc(p.name)} ${p.builtin ? '<small>Vorlage</small>' : ''}</span>
    <button class="btn btn--soft" data-load="${esc(p.name)}">Laden</button>${p.builtin ? '' : `<button class="btn btn--danger" data-del="${esc(p.name)}">Löschen</button>`}</li>`).join('');
  $$('#presets [data-load]').forEach(b => b.onclick = async () => {
    await api('POST', `/api/presets/${encodeURIComponent(b.dataset.load)}/load`); cfg = null; toast(`Profil „${b.dataset.load}“ geladen`); poll();
  });
  $$('#presets [data-del]').forEach(b => b.onclick = async () => { await api('DELETE', `/api/presets/${encodeURIComponent(b.dataset.del)}`); loadAdmin(); });
}
$('#preset-save').onclick = async () => {
  const name = $('#preset-name').value.trim(); if (!name) return;
  await api('POST', '/api/presets', { name }); $('#preset-name').value = ''; toast('Profil gesichert'); loadAdmin();
};

let settingsTimer = 0;
function saveSettings() { clearTimeout(settingsTimer); settingsTimer = setTimeout(() => api('PUT', '/api/settings', settings).then(() => toast('Gespeichert')), 500); }

function renderSchedule(presets) {
  const opts = [['', 'aktuelle Einstellungen'], ...presets.map(p => [p.name, p.name])];
  rowEditor($('#schedule'), settings.schedule, [
    { key: 'enabled', label: 'An', type: 'check' },
    { key: 'name', label: 'Name' },
    { key: 'days', label: 'Tage', type: 'days' },
    { key: 'date', label: 'oder einmalig am', placeholder: 'JJJJ-MM-TT' },
    { key: 'start', label: 'Start', placeholder: '09:00' },
    { key: 'stop', label: 'Stopp', placeholder: '10:00' },
    { key: 'preset', label: 'Profil', type: 'select', options: opts },
  ], saveSettings, 'Entfernen');
}
$('#add-schedule').onclick = () => {
  settings.schedule.push({ enabled: true, name: 'Sendung', date: '', days: [1, 2, 3, 4, 5], start: '09:00', stop: '10:00', preset: '' });
  renderSchedule([]); saveSettings(); loadAdmin();
};

const alarmLabels = {
  signal_loss_s: 'Kein Signal länger als (s)', no_peer_s: 'Keine Gegenstelle länger als (s)', min_bitrate_kbps: 'Bitrate unter (kbit/s)',
  max_lost_per_min: 'Verluste je Minute über', max_restarts: 'Neustarts in 10 min ab', cpu_percent: 'CPU über (%)',
  memory_percent: 'Speicher über (%)', disk_free_gb: 'Freier Platz unter (GB)',
  loudness_target: 'Ziel-Lautheit (LUFS)', loudness_tolerance_lu: 'Erlaubte Abweichung (LU)', silence_s: 'Spur still länger als (s)',
  true_peak_max_dbtp: 'True Peak über (dBTP)'
};
function renderAlarmFields() {
  const host = $('#alarm-fields'); host.innerHTML = '';
  for (const [k, label] of Object.entries(alarmLabels)) {
    const l = document.createElement('label'); l.className = 'field';
    const i = document.createElement('input'); i.className = 'input'; i.inputMode = 'numeric'; i.value = settings.alarms[k];
    i.oninput = () => { const v = parseFloat(i.value.replace(',', '.')); settings.alarms[k] = Number.isFinite(v) ? v : 0; saveSettings(); };
    const sp = document.createElement('span'); sp.textContent = label + (k === 'loudness_target' || k === 'true_peak_max_dbtp' ? '' : ' (0 = aus)');
    l.append(sp, i); host.append(l);
  }
}

const bytes = n => n > 1e9 ? `${fmt.format(n / 1e9)} GB` : `${fmt.format(n / 1e6)} MB`;
function renderRecordings(list) {
  $('#recordings').innerHTML = list.length ? list.map(f => `<li><span>${esc(f.name)} <small>${bytes(f.bytes)} · ${new Date(f.modified_ms).toLocaleString('de-DE')}</small></span>
    <a class="btn btn--soft" href="/api/recordings/${encodeURIComponent(f.name)}">Herunterladen</a><button class="btn btn--danger" data-del="${esc(f.name)}">Löschen</button></li>`).join('')
    : '<li class="empty">Noch keine Aufnahmen. Einschalten unter Betrieb → Mehr Einstellungen → Aufnahme parallel.</li>';
  $$('#recordings [data-del]').forEach(b => b.onclick = async () => { await api('DELETE', `/api/recordings/${encodeURIComponent(b.dataset.del)}`); loadAdmin(); });
}

function renderStandby(list) {
  const cur = cfg?.encoder.standby.image_path || '';
  $('#standby').innerHTML = list.map(f => `<figure><img src="/api/standby/${encodeURIComponent(f.name)}" alt="Standbild ${esc(f.name)}" loading="lazy">
    <figcaption>${esc(f.name)}${cur.endsWith('/' + f.name) && cfg.encoder.standby.pattern === 'image' ? ' · aktiv' : ''}</figcaption>
    <div class="actions"><button class="btn btn--soft" data-use="${esc(f.name)}">Verwenden</button><button class="btn btn--danger" data-del="${esc(f.name)}">Löschen</button></div></figure>`).join('')
    || '<p class="hint">Noch keine eigenen Standbilder – bis dahin kommen Farbbalken.</p>';
  $$('#standby [data-use]').forEach(b => b.onclick = async () => { await api('POST', `/api/standby/${encodeURIComponent(b.dataset.use)}/use`); cfg = null; toast('Standbild gesetzt'); await poll(); loadAdmin(); });
  $$('#standby [data-del]').forEach(b => b.onclick = async () => { await api('DELETE', `/api/standby/${encodeURIComponent(b.dataset.del)}`); loadAdmin(); });
}
$('#standby-file').onchange = async e => {
  const f = e.target.files[0]; if (!f) return;
  const r = await fetch(`/api/standby?name=${encodeURIComponent(f.name)}`, { method: 'POST', body: f });
  toast(r.ok ? 'Standbild hochgeladen' : 'Nur PNG oder JPEG'); e.target.value = ''; loadAdmin();
};

function renderUsers(list, me) {
  $('#me').textContent = me.open ? 'offen, ohne Anmeldung' : me.name ? `angemeldet: ${me.name}` : '';
  $('#logout').hidden = !me.name;
  const roles = { admin: 'Verwalten', operator: 'Bedienen', observer: 'Ansehen' };
  $('#users').innerHTML = list.length ? list.map(u => `<li><span>${esc(u.name)} <small>${roles[u.role]}</small></span><button class="btn btn--danger" data-del="${esc(u.name)}">Entfernen</button></li>`).join('')
    : '<li class="empty">Keine Benutzer.</li>';
  $$('#users [data-del]').forEach(b => b.onclick = async () => { await api('DELETE', `/api/users/${encodeURIComponent(b.dataset.del)}`); loadAdmin(); });
}
$('#u-add').onclick = async () => {
  await api('POST', '/api/users', { name: $('#u-name').value, password: $('#u-pw').value, role: $('#u-role').value });
  $('#u-pw').value = ''; toast('Benutzer angelegt'); loadAdmin();
};

function renderChart(points) {
  if (points.length < 2) { $('#chart').innerHTML = '<p class="hint">Sammelt Werte, sobald der Dienst läuft (alle 5 s).</p>'; return; }
  const W = 600, H = 140, P = 24;
  const maxB = Math.max(1, ...points.map(p => p.bitrate_kbps));
  const maxR = Math.max(1, ...points.map(p => p.rtt_ms || 0));
  const x = i => P + (i / (points.length - 1)) * (W - 2 * P);
  const path = (f, max) => points.map((p, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${(H - P - (f(p) / max) * (H - 2 * P)).toFixed(1)}`).join('');
  $('#chart').innerHTML = `<svg viewBox="0 0 ${W} ${H}" role="img" aria-label="Bitrate und RTT der letzten Stunde">
    <path class="line" d="${path(p => p.bitrate_kbps, maxB)}"/><path class="line2" d="${path(p => p.rtt_ms || 0, maxR)}"/>
    <text class="axis" x="${P}" y="14">Bitrate bis ${fmt.format(maxB / 1000)} Mbit/s</text>
    <text class="axis" x="${W - P}" y="14" text-anchor="end">RTT bis ${fmt.format(maxR)} ms</text></svg>`;
}

// ---------------- Laufend: Alarme, System, Tonspuren
function renderOps(st) {
  const al = st.alarms || [];
  $('#alarm-banner').hidden = !al.length;
  $('#alarm-banner').textContent = al.map(a => a.text).join(' · ');
  $('#alarms').innerHTML = al.length ? al.map(a => `<li><span class="chip" data-s="${a.level === 'error' ? 'error' : 'warn'}">${esc(a.text)}</span><small>seit ${new Date(a.since_ms).toLocaleTimeString('de-DE')}</small></li>`).join('')
    : '<li class="empty">Keine Alarme.</li>';
  const s = st.system || {};
  $('#system').innerHTML = [
    tile('CPU', fmt0.format(s.cpu_percent || 0), '%', s.cpu_percent > 90 ? 'warn' : ''),
    tile('Arbeitsspeicher', fmt0.format(s.memory_percent || 0), '%'),
    tile('Frei für Aufnahmen', fmt.format(s.disk_free_gb || 0), 'GB'),
    tile('Rechner', esc(s.hostname || dash)),
    tile('Läuft seit', dur(s.uptime_s)),
  ].join('');
  $('#rec-free').textContent = s.disk_free_gb ? `${fmt.format(s.disk_free_gb)} GB frei` : '';
  const tr = st.runtime.status?.audio_tracks || [];
  $('#dec-tracks').textContent = st.config.role === 'decoder' && tr.length
    ? 'Empfangene Spuren: ' + tr.map(t => `${t.index}: ${t.language || '?'}${t.selected ? ' (spielt)' : ''}`).join(' · ') : '';
  const u = st.user;
  if (!u.open && !u.name) showLogin();
}
const _render = render;
render = function (st) { _render(st); renderOps(st); };

$('#auto-delete').onchange = e => { settings.delete_oldest_recordings = e.target.checked; saveSettings(); };

// ---------------- Lautheit je Spur
function renderMeters(st) {
  const tr = (st.runtime.status?.audio_tracks || []).filter(t => st.config.role === 'encoder' || t.selected);
  $('#audio-card').hidden = !tr.length;
  if (!tr.length) return;
  const target = st.loudness_target ?? -23;
  const pos = v => Math.max(0, Math.min(100, (v + 60) / 60 * 100));
  const f = v => v == null ? dash : fmt.format(v);
  $('#meters').innerHTML = tr.map(t => {
    const l = t.loudness || {};
    const m = l.momentary;
    const tg = t.target ?? target;
    const state = m == null ? '' : (l.true_peak > -1 ? 'error' : Math.abs((l.short_term ?? m) - tg) > 5 ? 'warn' : '');
    return `<div class="meter"><header>${esc(t.name || 'Spur ' + (t.index + 1))} <small>${esc(t.language || '')}${l.silent_s >= 2 ? ' · still' : ''}</small></header>
      <div class="bar" role="meter" aria-label="Momentary-Lautheit" aria-valuemin="-60" aria-valuemax="0" aria-valuenow="${m ?? -60}"><span data-s="${state}" style="inline-size:${m == null ? 0 : pos(m)}%"></span><i style="inset-inline-start:${pos(tg)}%"></i></div>
      <div class="vals"><span>M <b>${f(m)}</b></span><span>S <b>${f(l.short_term)}</b></span><span>I <b>${f(l.integrated)}</b> LUFS</span><span>LRA <b>${f(l.lra)}</b> LU</span><span>TP <b>${f(l.true_peak)}</b> dBTP</span>${t.leveler_gain ? `<span>Regelung <b>${fmt.format(t.leveler_gain[0] + t.leveler_gain[1])}</b> dB</span>` : ''}</div></div>`;
  }).join('');
}
const _render2 = render;
render = function (st) { _render2(st); renderMeters(st); };

// ---------------- Untertitel
$('#caption-send').addEventListener('submit', async e => {
  e.preventDefault();
  const text = $('#caption-text').value.trim(); if (!text) return;
  await api('POST', '/api/caption', { text }); $('#caption-text').value = '';
});
function renderCaptions(st) {
  const s = st.runtime.status, enc = st.config.role === 'encoder';
  $('#caption-send').hidden = !(enc && s && st.config.encoder.captions.source === 'live');
  const cc = s?.captions;
  $('#chip-cc').hidden = !cc?.present;
  const line = $('#caption-line');
  line.hidden = !(cc?.text && cc.present);
  if (cc?.text) line.textContent = cc.text;
}
const _render3 = render;
render = function (st) { _render3(st); renderCaptions(st); };

// ---------------- Whisper-Modelle
function renderModels(list) {
  $('#models').innerHTML = list.map(m => `<li><span>${esc(m.name)} <small>${bytes(m.bytes)}${m.present ? ' · vorhanden' : m.downloading ? ` · lädt ${m.loaded_bytes ? Math.round(m.loaded_bytes * 100 / m.bytes) : 0} %` : ''}</small></span>
    ${m.present ? `<button class="btn btn--danger" data-mdel="${m.name}">Löschen</button>` : m.downloading ? '' : `<button class="btn btn--soft" data-mget="${m.name}">Laden (${bytes(m.bytes)})</button>`}</li>`).join('');
  $$('#models [data-mget]').forEach(b => b.onclick = async () => { await api('POST', `/api/models/${b.dataset.mget}/download`); toast('Modell wird geladen'); setTimeout(loadAdmin, 1500); });
  $$('#models [data-mdel]').forEach(b => b.onclick = async () => { await api('DELETE', `/api/models/${b.dataset.mdel}`); loadAdmin(); });
}
function renderAsr(st) {
  const a = st.runtime.status?.captions?.asr;
  if (!a) return;
  const sub = $('#stage-sub');
  sub.textContent += ` · Spracherkennung: ${a.state}${a.last_delay_ms != null ? `, Verzug ${fmt.format(a.last_delay_ms / 1000)} s` : ''}`;
}
const _render4 = render;
render = function (st) { _render4(st); renderAsr(st); };

// ---------------- Grafik
function renderGfx(st) {
  const g = st.runtime.status?.graphics, r = st.renderer;
  const h = $('#gfx-hint'); if (!h) return;
  h.textContent = st.config.encoder.graphics.burn === 'off' ? '' :
    (r?.connected ? 'Renderer verbunden' : 'Kein Renderer verbunden – Untertitel erscheinen als schlichte Texteinblendung')
    + (g ? ` · Bild-Port ${g.frame_port}${g.graphic_visible ? ' · Grafik im Bild' : ''}` : '');
}
const _render5 = render;
render = function (st) { _render5(st); renderGfx(st); };

// ---------------- PDF-Bericht mit Zeitraum
function repLink() {
  const q = new URLSearchParams();
  if ($('#rep-from').value) q.set('from', $('#rep-from').value);
  if ($('#rep-to').value) q.set('to', $('#rep-to').value);
  $('#rep-pdf').href = '/api/report.pdf' + (q.toString() ? '?' + q : '');
}
$('#rep-from').addEventListener('change', repLink);
$('#rep-to').addEventListener('change', repLink);
