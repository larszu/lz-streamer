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
- **Latenz heute** [M], Testbild → SRT → Decoder-Ausgang am M1 Pro, im LAN, eigener Mux (Vorgabe):

  | SRT-Latenz | Median | mit mpegtsmux |
  |---|---|---|
  | 120 ms (Demux 40, Reserve 60) | 204–214 ms | 241–249 ms |
  | 40 ms (Demux 0, Reserve 20) | 114–132 ms, Mittel der Mediane 127 ms | 176–193 ms |

  Glas-zu-Glas mit Capture, Ausgabe und Monitor gerechnet [S]: **≈ 160–170 ms** bei SRT 40 ms, siehe §5.
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

Gemessen am MacBook Pro M1 Pro: Testbild 1280×720p50, VideoToolbox H.264, SRT über 127.0.0.1, Decoder avdec_h264. Je Zeile mehrere Läufe mit `lz-streamer latenz --runden 7`, angegeben ist die Spanne der Mediane. Gemessen wird beim Zeigen an der taktsynchronen Senke; Bilder, die mehr als 20 ms zu spät kommen, verwirft sie wie ein Bildschirm (gezählt, in allen Zeilen 0–3 bei rund 750 Bildern). Auf dem Rechner liefen dabei andere Bauten (Last 8–27), die Werte streuen deshalb um ±15 ms.

| Einstellung | Mux | Quelle | Encode | Mux | SRT+Demux+Decode | Ausgabe | **Summe** |
|---|---|---|---|---|---|---|---|
| SRT 40, Demux 0, Reserve 20 | mpegtsmux | 20–33 | 4–9 | 54–65 | 35–57 | 18–51 | **176–193** |
| dito | eigener (`lztsmux`) | 25–34 | 9 | 0 | 64–71 | 11–33 | **114–138** |
| dito, ohne Ton | eigener | 27 | 9 | 0 | 64 | 8 | **109** |
| dito, x264 | mpegtsmux | 24 | 2 | 31 | 48 | 38 | **148** |
| dito, x264 | eigener | 27 | 2 | 0 | 70 | 32 | **131** |
| dito, CBR 12 000 kbit/s | mpegtsmux mit PCR-Takt | 30 | 4 | 54 | 62 | 47 | **192** |
| dito, CBR 12 000 kbit/s | eigener | 27–36 | 9 | 0 | 80–86 | 48–71 | **166–193** |
| Vorgabe: SRT 120, Demux 40, Reserve 60 | mpegtsmux | 25–28 | 9 | 47–52 | 143–155 | 0 | **241–249** |
| dito | eigener | 28–32 | 9 | 0 | 146–149 | 19–22 | **204–214** |

Lesart:
- **Quelle** (20–35 ms) sind gut ein Bildtakt bei 50p plus der Umschalter.
- **Encode** ist mit 2–9 ms klein.
- **Mux:** `mpegtsmux` ist ein Aggregator. Er gibt ein Bild erst ab, wenn auch der Ton einen Puffer mit späterer Zeit geliefert hat (der Ton kommt ≈ 45–55 ms nach seinem Zeitstempel). Mit `alignment=7` hält er außerdem die letzten bis zu sechs Pakete jedes Bilds bis zum nächsten Bild zurück (+1 Bildtakt). Die PCR setzt er fest 125 ms vor die DTS.
- **Eigener Mux:** schreibt jedes Bild sofort (Station 0 ms) und nimmt die PCR aus der Pipeline-Uhr beim Senden. Der Abstand PCR → PTS ist der einzige Puffer; der Mux lernt ihn aus der spätesten Eingangsverspätung (meist der Ton) plus SRT-Latenz plus 5 ms und gleitet zurück, wenn er größer ist als nötig. Gelernt: 93–98 ms bei SRT 40, 70 ms ohne Ton, 177 ms bei SRT 120.
- **SRT+Demux+Decode** wächst mit dem eigenen Mux, weil der Decoder jetzt auf die PTS wartet statt auf den Mux. In Summe bleibt der Gewinn: 50–60 ms bei SRT 40, 35–45 ms bei SRT 120.
- **CBR:** Bei konstanter Rate muss jedes Bild durch die feste Rate; große Bilder (Szenenwechsel) brauchen bei 12 Mbit/s 20–40 ms. Der Mux rechnet die Warteschlange in den Puffer ein. CBR ist deshalb nicht schneller als `mpegtsmux` mit PCR-Takt, aber T-STD-sauber (siehe §5.4).
- **Ausgabe** ist die Wartezeit an der Senke; sie ist der Rest des Puffers. Bei 0 kamen Bilder knapp oder zu spät.

### 5.2 Rechenmodell Glas-zu-Glas [S]

Bei 50p ist ein Bild T = 20 ms lang.

```
G2G = Capture (≈ T) + Skalierung (2–5) + Encode (3–20) + Mux (0 mit lztsmux / 50–65 mit mpegtsmux)
    + Netz einweg (d) + SRT-Latenz (L ≥ 3–4 × RTT, im LAN ≥ 20–40)
    + Demux (0–40) + Decode (5–10, vtdec_hw +100/+330) + Warten auf Ausgabetakt (0–T, Mittel T/2)
    + DeckLink-Ausgabe (≈ T) + Monitor (5–15, TV 30–100)
```

| Szenario | Netz einweg | RTT | SRT-Latenz | Heute (Mac, gemessener Weg + Capture/Ausgabe/Monitor) | Optimiert (Linux/NVENC) |
|---|---|---|---|---|---|
| Gleicher Raum/LAN | 0,5 | 1 | 40 | **≈ 160–170 ms** | **≈ 120–135 ms** |
| Inland Internet | 15 | 30 | 120 | ≈ 290 ms | ≈ 215 ms |
| Europa | 30 | 60 | 240 | ≈ 425 ms | ≈ 350 ms |
| Bangkok → Köln, Internet | 90 | 180 | 720 | ≈ 1 000 ms | ≈ 925 ms |
| Bangkok → Köln, Premium-Route + FEC/2022-7 ohne ARQ | 65 | 130 | – (Jitter/FEC 20) | nicht möglich (kein FEC) | ≈ 170 ms; 120 ms nur mit Sub-Frame-Capture/-Ausgabe und Genlock |
| UHD 2160p50 HEVC am M1 Pro | | | | +≈ 20 ms (Encode nahe einem Bildtakt) | – |
| H.265-Decode mit vtdec_hw | | | | +≈ 330 ms [M] | NVDEC: zu messen |

Rechenweg „heute, LAN“: 127 ms [M, Quelle bis Senke, eigener Mux] + 20 (Capture) + 20 (DeckLink-Ausgabe) + 10 (Monitor) − 10 (die Testbildquelle enthält schon einen halben Takt) ≈ 167 ms. Mit `mpegtsmux` wären es ≈ 230 ms.

Rechenweg „optimiert, LAN“: 20 + 3 + 6 + 5 + 0,5 + 40 + 10 + 8 + 10 + 20 + 10 ≈ 133 ms. Mit 30 ms SRT und 0 ms Demux ≈ 120 ms.

**Folgen:**
- Im LAN sind 120–135 ms erreichbar. Der eigene Mux ohne Wartezeit ist da; dazu kommen der Software-Decoder für ≤ 1080p, eine kurze SRT-Latenz und eine schnellere Plattform (Encode, Capture).
- Über das Internet bestimmt fast nur die SRT-Latenz (3–4 × RTT) das Ergebnis.
- Die 120 ms Bangkok → Köln aus dem Konzept gehen nur mit FEC statt Wiederholung, zwei Netzwegen und Sub-Frame-Pfad. Mit SRT allein ist das physikalisch nicht machbar.

### 5.3 Wie nachmessen

```bash
./target/release/lz-streamer latenz --srt-latency 40 --demux 0 --reserve 20 --runden 7
./target/release/lz-streamer latenz --srt-latency 40 --demux 0 --reserve 20 --mux mpegtsmux   # Vergleich
./target/release/lz-streamer latenz --srt-latency 40 --demux 0 --reserve 20 --mux-verzug 90  # fester Puffer
./target/release/lz-streamer latenz --encoder x264enc --h265 --decoder avdec_h265
```

Die Ausgabe nennt die am Decoder verworfenen Bilder und den Puffer PCR → PTS, den der eigene Mux gelernt hat.

Glas-zu-Glas über eine echte Strecke kommt mit dem Messkettenbaustein aus lz-scopes (§2): Stempel-Testbild am Encoder, Kamera auf den Decoder-Monitor oder Rückweg in lz-scopes.

### 5.4 TR 101 290 und T-STD [M]

`src/tsanalyse.rs` prüft einen mitgeschnittenen TS nach ETSI TR 101 290:

- **Priorität 1:** Sync-Byte, PAT (Abstand ≤ 0,5 s, table_id, Verschlüsselung), CC, PMT (≤ 0,5 s), PID fehlt > 5 s.
- **Priorität 2:** Transport-Error-Bit, CRC von PAT/PMT, PCR-Abstand ≤ 40 ms, PCR-Sprung ≤ 100 ms ohne Kennzeichen, PCR-Genauigkeit ±500 ns gegen die Rate, PTS-Abstand ≤ 700 ms.
- **T-STD:** Transportpuffer TB_n mit 512 Byte je Elementarstrom und der Abflussrate nach ISO/IEC 13818-1 (Ton 2 Mbit/s, Bild 1,2 × Rmax).

Ergebnis, Testbild 640×360p25, 1500 kbit/s, Ton AAC, je 4–6 s Mitschnitt nach dem Anlauf:

| Strom | Prio 1 | Prio 2 | T-STD-Überläufe | PCR-Abstand | PAT/PMT | PCR-Genauigkeit | TSDuck `continuity`, `pcrverify --jitter-max 1` |
|---|---|---|---|---|---|---|---|
| eigener Mux, CBR 3000 kbit/s | 0 | 0 | 0 | 20 ms | 100 ms | 0 ns (exakt aus der Sendeposition) | keine Fehler, 261 PCR ohne Jitter |
| eigener Mux, variabel, H.264 und H.265 | 0 | 0 (ohne Genauigkeit, gilt nur bei CBR) | – | ≤ 40 ms | 100 ms | – | – |

Der CBR-Takt schickt ein Tonpaket erst, wenn es den Transportpuffer nicht überlaufen lässt. Bei 12 Mbit/s liegen dadurch zwischen zwei Tonpaketen mindestens ≈ 0,75 ms.
