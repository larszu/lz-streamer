# Vergleich, Hardware-Empfehlung und Latenz-Budget

Stand: 05.11.2026. Grundlagen:
- `haivision-kb-analyse/docs/nachbau-konzept.md` (Marktvergleich §2, Bausteine §3, Stückliste §8)
- `lynx-analyse/docs/pec-1464-analyse.md`
- Bestandsaufnahme `lz-scopes`
- eigene Messungen mit `lz-streamer latenz`

Legende: **[M]** selbst gemessen · **[H]** Herstellerangabe, nicht geprüft · **[S]** Schätzung/Rechnung.
Preise sind Richtwerte aus Händlerlisten des Konzepts und nicht frisch geprüft.

---

## 1. Kurzfassung

- **Funktional** fehlen gegenüber den teuren Geräten fünf Dinge:
  - 4:2:2 10-bit und HDR-Kennzeichnung
  - Mehrkanal mit synchronen Ausgängen
  - Redundanz (SRT-Gruppen, FEC/2022-7)
  - Broadcast-TS mit SCTE/CC
  - Aufnahme parallel zum Stream
- **lz-scopes hat schon viele Messbausteine**, die der Streamer braucht:
  - Latenz- und A/V-Messung mit Blitz/Piep und Zeitstempeln
  - Encoder-Qualitätsprüfung (Blocking, Banding, Verzögerung)
  - VANC-Parser für Timecode, HDR-Metadaten, AFD und CEA-708
  - Genlock-/Referenzanzeige, PTP-Uhr
  - Ringpuffer-Aufnahme, Companion-Steuerung

  Übernehmen statt neu bauen.
- **Latenz heute** [M], Testbild → SRT → Decoder-Ausgang am M1 Pro, im LAN:

  | SRT-Latenz | Median |
  |---|---|
  | 120 ms | 211–227 ms |
  | 40 ms | 134–156 ms |

  Glas-zu-Glas mit Capture, Ausgabe und Monitor gerechnet [S]: **≈ 190–210 ms**. Optimiert sind im LAN **≈ 120–135 ms** realistisch, siehe §5.
- **Hardware-Empfehlung:**
  - Hauptgerät: Linux-Kompakt-PC mit NVIDIA-Blackwell-GPU und DeckLink 8K Pro (12G, Referenzeingang).
  - Mobil: Mac mini M4 Pro mit UltraStudio 4K Mini (12G).
  - Der vorhandene Recorder 3G bleibt für HD-Einsätze.

---

## 2. Was lz-scopes schon hat und der Streamer braucht

| Baustein in lz-scopes | Datei | Nutzen im Streamer | Aufwand |
|---|---|---|---|
| Latenzmessung mit gestempelten Testbildern, Stationen einzeln | `src/latency.ts`, `server/stamp.mjs` | Glas-zu-Glas-Messung über echte Strecke statt nur im Prozess (§5.8 Konzept verlangt eine Messkette ab MVP) | klein – Stempel ins Testbild, Auswertung am Rückweg |
| A/V-Versatz per Blitz und Piep (BT.1359) | `src/audio/dsp/avsync.ts` | Lippensynchronität des Streams messen, Abnahme für Langläufe | klein |
| Encoder-Vertrauen: Blocking (Wang/Bovik), Banding (CAMBI), Verzögerung, A/V | `src/encoder.ts`, `docs/research/encoder-confidence.md` | Bitrate/Encoder-Wahl mit Zahlen belegen; Ampel „Bild leidet“ am Rückweg | mittel – Rückweg-Decoder füttert die Rechnung |
| Lautheit (EBU R 128), Pegel | `src/audio` | Tonpegel-Anzeige im Streamer (heute keine) | klein |
| VANC-Parser: ST 352, ST 2108 HDR, ST 12 Timecode, AFD, CEA-708/ST 299-Erkennung | `src/vanc.ts` | Eingang beschreiben (HDR? Timecode? CC vorhanden?), Grundlage für Durchreichen | mittel – DeckLink-VANC im GStreamer-Weg abgreifen |
| Genlock/Referenz-Status der DeckLink, Bildphase gegen ST 2059 | `src/genlock.ts`, `server/phase.mjs` | Decoder-Ausgabe auf Haustakt | mittel (ungeprüft an Hardware) |
| PTP/Uhr, Timecode-Anzeige | `server/ptp.mjs`, `src/clock` | gemeinsame Zeitbasis beider Enden für Latenzmessung | klein |
| Ringpuffer-Aufnahme, synchrones Abspielen | `server/ring.mjs`, `server/record.mjs` | Aufnahme parallel zum Stream, Instant-Replay bei Störung | mittel |
| Testbilder (SMPTE, PLUGE, HDR, Zonenplatte) | `src/patterns.ts`, `patterns16.ts` | Bessere Standbild- und Messbilder als `videotestsrc` | klein |
| SRT/RTMP/RTSP-Eingänge über die Bridge | `server/ffmpeg.mjs`, `rtsp.mjs` | lz-scopes kann den Rückweg des Streamers direkt messen; umgekehrt als Quelle | keiner – heute nutzbar |
| Steuer-API + Companion-Modul | `docs/control-api.md` | Muster für Streamer-API und Companion-Tasten (Start/Stopp/Standbild) | klein |
| DeckLink-Einrichtungsdialog, p/PsF-Erkennung | `src/decklinkSetup.ts` | Recorder-3G-Fallen sind dort gelöst (Formatflattern 1080p25/PsF, 1–4 s bis Bild) | klein – Wissen übernehmen |

**Folgerung:** Messen bleibt in lz-scopes. Der Streamer bekommt ein Testbild mit Stempel und einen Rückweg, den lz-scopes auswertet. Doppelt bauen wäre falsch.

---

## 3. Was die anderen können, was lz-streamer noch nicht kann

| Funktion | lz-streamer 0.1 | Lynx PEC-1464 | AJA HELO Plus | AJA BRIDGE LIVE | Haivision Makito X4 | Matrox Monarch EDGE |
|---|---|---|---|---|---|---|
| 12G-SDI-Eingang | Weg vorhanden, ungeprüft (nur 3G-Gerät da) | ja [H] | nein (3G) [H] | ja [H] | ja [H] | ja (4K-Variante) [H] |
| UHD-Encode | ja, HEVC (am M1 Pro Grenzlast bei 50p) [M] | ja, nur 4:2:0 [H] | nein (bis 1080p60) [H] | ja [H] | ja, bis 2160p60 [H] | ja [H] |
| HD → UHD hochskalieren | ja, CPU-Lanczos, 50p in Echtzeit [M] | **nein**, nur herunter [H] | nein [H] | nicht belegt | nicht belegt | nicht belegt |
| 4:2:2 10-bit | nein | nur bis 1080p [H] | nein | ja [H] | ja [H] | ja (10-bit-Variante) [H] |
| HDR-Kennzeichnung (PQ/HLG) | nein | nein (laut Handbuch kein ST 2084) [H] | nein | ja [H] | ja [H] | nicht belegt |
| Mehrere Kanäle / Streams | 1 | 4 Encoder, 4 Streams [H] | 2 (Stream + Aufnahme) [H] | mehrere [H] | 1 × 4K oder 4 × HD, synchron [H] | 4 × HD [H] |
| SRT Caller/Listener | ja | ja + Rendezvous [H] | ja [H] | ja [H] | ja [H] | ja [H] |
| SRT-FEC / Redundanz | nein | FEC [H] | nein | nicht belegt | Path Redundancy [H] | nicht belegt |
| TS über UDP/RTP | nein | ja, mit FEC [H] | nein | ja [H] | ja [H] | ja [H] |
| RTMP(S) | RTMP | ja [H] | ja [H] | ja [H] | ja [H] | ja [H] |
| HLS/RTSP/NDI | nein | HLS-Push, RTSP [H] | nein | NDI [H] | nein | nicht belegt |
| Aufnahme parallel | nein | USB, MP4/MOV/TS [H] | SD/USB/Netz [H] | nicht belegt | nicht belegt | nicht belegt |
| Overlays, Bild-in-Bild | nein | Overlays [H] | Layouts, Grafik [H] | nicht belegt | nein | nicht belegt |
| SCTE-35, CEA-608/708, KLV | nein | nicht belegt | nein | nicht belegt | ja [H] | teils [H] |
| Decoder-Rolle | ja (Fenster/DeckLink) | nein | nein | ja [H] | eigener Makito-Decoder | Rückweg [H] |
| Standbild bei Signalverlust | ja [M] | Fallback-Bild [H] | nicht belegt | nicht belegt | ja [H] | nicht belegt |
| Fernverwaltung/Cloud | nein | nur Web-UI, SNMP lesend [H] | REST [H] | nicht belegt | Haivision Hub [H] | nicht belegt |
| Latenz Encode | ≈ 9 ms VideoToolbox, 3 ms x264 bei 720p [M] | keine Angabe | keine Angabe | nicht belegt | „< 50 ms“ laut Händler [H] | nicht belegt |
| Preis (Richtwert) | Hardware 830–2 650 € | nicht recherchiert | 1 800–2 100 € | nicht recherchiert | 6 000 $ (1 Kanal) | 5 400–10 700 $ |

Bonding über Mobilfunk (Teradek Prism mit Core, LiveU) fehlt ebenfalls. Das löst lz-streamer nicht selbst. SRT-Gruppen über zwei Router (Kabel + 5G) sind der offene Weg dorthin.

**Wo lz-streamer heute schon gleichauf oder besser ist:**
- Hochskalieren, das die Lynx gar nicht kann
- Encoder und Decoder in einer Software
- Hardware-Encoder jeder Plattform
- gemessene Latenz statt Werbeangabe
- keine Lizenzstufen

**Reihenfolge, die den größten Abstand schließt:**
1. Aufnahme parallel zum Stream (klein, häufig gebraucht).
2. Messkette mit lz-scopes (klein, macht alles Weitere belegbar).
3. 4:2:2 10-bit und HDR-Kennzeichnung (mittel, braucht NVENC Blackwell oder VideoToolbox-Profil).
4. SRT-Gruppen und FEC (mittel). Erst damit wird Contribution über das Internet ernsthaft.
5. Mehrkanal (mittel).
6. Broadcast-TS mit SCTE/CC (groß, nur wenn ein Kunde IRD-Abnahme fordert).

---

## 4. Hardware-Empfehlung

### A · Hauptgerät Studio/Ü-Wagen (Linux)

| Teil | Wahl | Warum | ca. € |
|---|---|---|---|
| Rechner | Kompakt-PC Ryzen 7 / Core Ultra 7, 32 GB, NVMe | Luft für Skalierung und Mehrkanal | 800 |
| GPU | NVIDIA RTX 5060 Ti oder RTX PRO 2000 (Blackwell) | NVENC mit 4:2:2 10-bit H.264/HEVC ab Blackwell; CUDA-Skalierer statt CPU | 450–700 |
| Capture | **Blackmagic DeckLink 8K Pro** (4 × 12G-SDI, Referenzeingang) | 12G-UHD, mehrere HD-Kanäle, Genlock für Decoder-Rolle | ≈ 1 300 |
| Netz | 2. NIC 2,5/10 GbE | SRT-Gruppen über zwei Wege | 50–150 |
| **Summe** | | | **≈ 2 600–2 950** |

Günstiger ohne 12G: DeckLink Duo 2 (4 × 3G) ≈ 550 €.

### B · Mobil (Mac)

| Teil | Wahl | Warum | ca. € |
|---|---|---|---|
| Rechner | Mac mini M4 Pro | stärkere Media-Engine als der M1 Pro, an dem UHD50-HEVC an der Grenze lief [M] | ≈ 1 600 |
| Capture 12G | **UltraStudio 4K Mini** (Thunderbolt, 12G-SDI/HDMI) | einziges Blackmagic-Thunderbolt-Gerät mit 12G im Kleinformat | ≈ 1 000 (prüfen) |
| Capture HD | UltraStudio Recorder 3G (vorhanden) | bleibt für HD; mehr als 1080p kann er nicht | – |

### C · Decoder-Seite

Bauform wie A, aber mit kleinerer GPU. Wichtig sind zwei Dinge: eine DeckLink mit Referenzeingang (8K Pro, ≈ 1 300 €) für Genlock und ein Monitor mit Spielmodus. Ein TV mit Bildverbesserung kostet 30–100 ms.

### Hochskalieren

- **Software (heute):** CPU-Lanczos mit 8 Threads schafft HD → UHD 50p am M1 Pro [M].
- **Auf Linux:** CUDA-Skalierung in GStreamer (`cudaconvertscale` aus nvcodec) entlastet die CPU. Noch einbauen und messen.
- **Hardware nur bei Broadcast-Anspruch** (bewegungsadaptiv, Cross-Conversion 1080i ↔ 720p ↔ 2160p, HDR-Mapping): Blackmagic Teranex, AJA FS-HDR, Lynx greenMachine titan. Kein Muss für Streaming.

### Prüfmittel

- TSDuck (frei)
- lz-scopes als Rückweg-Monitor
- ein Blitz-/Piep-Messbild
- für die Abnahme ein Leih-IRD

---

## 5. Latenz

### 5.1 Gemessen, aufgeschlüsselt [M]

Gemessen am MacBook Pro M1 Pro: Testbild 1280×720p50, VideoToolbox H.264, SRT über 127.0.0.1, Decoder avdec_h264. Median aus 7 Runden mit `lz-streamer latenz`.

| Einstellung | Quelle | Encode | Parser+Mux | SRT+Demux+Decode | Ausgabe | **Summe** |
|---|---|---|---|---|---|---|
| Vorgabe: SRT 120, Demux 40, Reserve 60 | 30 | 9 | 48 | 128 | 0 | **211** |
| SRT 40, Demux 0, Reserve 20 | 25 | 9 | 51 | 50 | 13 | **155** |
| dito, x264 statt VideoToolbox | 27 | 3 | 37 | 43 | 24 | **134** |
| dito, ohne Ton | 28 | 9 | 40 | 44 | 22 | **140** |

Lesart:
- **Quelle** (25–30 ms) sind gut ein Bildtakt bei 50p plus der Umschalter.
- **Encode** ist mit 3–9 ms klein.
- **Parser+Mux** ist mit 37–51 ms der größte vermeidbare Posten. `mpegtsmux` wartet auf Ton und auf volle Paketgruppen. Schiebt man nur dort, wandert die Zeit an der taktsynchronen Senke nach hinten. Gewinnen lässt sie sich erst mit einem eigenen PCR-Pacer (Fahrplan Broadcast-TS).
- **SRT** geht 1 : 1 ein. Die doppelte Zählung durch `srtsrc` ist schon ausgeglichen.
- Unter ≈ 130 ms kommt dieser Weg ohne Umbau des Mux nicht.

### 5.2 Rechenmodell Glas-zu-Glas [S]

Bei 50p ist ein Bild T = 20 ms lang.

```
G2G = Capture (≈ T) + Skalierung (2–5) + Encode (3–20) + Mux (5 Ziel / 40–50 heute)
    + Netz einweg (d) + SRT-Latenz (L ≥ 3–4 × RTT, im LAN ≥ 20–40)
    + Demux (0–40) + Decode (5–10, vtdec_hw +100/+330) + Warten auf Ausgabetakt (0–T, Mittel T/2)
    + DeckLink-Ausgabe (≈ T) + Monitor (5–15, TV 30–100)
```

| Szenario | Netz einweg | RTT | SRT-Latenz | Heute (Mac, gemessener Weg + Capture/Ausgabe/Monitor) | Optimiert (Linux/NVENC, eigener Mux) |
|---|---|---|---|---|---|
| Gleicher Raum/LAN | 0,5 | 1 | 40 | **≈ 190–205 ms** | **≈ 120–135 ms** |
| Inland Internet | 15 | 30 | 120 | ≈ 290 ms | ≈ 215 ms |
| Europa | 30 | 60 | 240 | ≈ 425 ms | ≈ 350 ms |
| Bangkok → Köln, Internet | 90 | 180 | 720 | ≈ 1 000 ms | ≈ 925 ms |
| Bangkok → Köln, Premium-Route + FEC/2022-7 ohne ARQ | 65 | 130 | – (Jitter/FEC 20) | nicht möglich (kein FEC) | ≈ 170 ms; 120 ms nur mit Sub-Frame-Capture/-Ausgabe und Genlock |
| UHD 2160p50 HEVC am M1 Pro | | | | +≈ 20 ms (Encode nahe einem Bildtakt) | – |
| H.265-Decode mit vtdec_hw | | | | +≈ 330 ms [M] | NVDEC: zu messen |

Rechenweg „heute, LAN“: 155 ms [M, Quelle bis Senke] + 20 (Capture) + 20 (DeckLink-Ausgabe) + 10 (Monitor) − 10 (die Testbildquelle enthält schon einen halben Takt) ≈ 195 ms.

Rechenweg „optimiert, LAN“: 20 + 3 + 6 + 5 + 0,5 + 40 + 10 + 8 + 10 + 20 + 10 ≈ 133 ms. Mit 30 ms SRT und 0 ms Demux ≈ 120 ms.

**Folgen:**
- Im LAN sind 120–135 ms erreichbar. Dafür braucht es drei Dinge: einen eigenen PCR-gepacten Ausgang statt `mpegtsmux`-Wartezeit, den Software-Decoder für ≤ 1080p und eine kurze SRT-Latenz.
- Über das Internet bestimmt fast nur die SRT-Latenz (3–4 × RTT) das Ergebnis.
- Die 120 ms Bangkok → Köln aus dem Konzept gehen nur mit FEC statt Wiederholung, zwei Netzwegen und Sub-Frame-Pfad. Mit SRT allein ist das physikalisch nicht machbar.

### 5.3 Wie nachmessen

```bash
./target/release/lz-streamer latenz --srt-latency 40 --demux 0 --reserve 20 --runden 7
./target/release/lz-streamer latenz --encoder x264enc --h265 --decoder avdec_h265
```

Glas-zu-Glas über eine echte Strecke kommt mit dem Messkettenbaustein aus lz-scopes (§2): Stempel-Testbild am Encoder, Kamera auf den Decoder-Monitor oder Rückweg in lz-scopes.
