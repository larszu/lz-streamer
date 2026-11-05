# lz-streamer

*English summary: a single-channel SRT video encoder/decoder in Rust + GStreamer with a one-screen web UI. Hardware encoders first (VideoToolbox, NVENC, VA-API/QSV), x264/x265 as fallback, standby picture on signal loss without dropping the output, and a supervisor that restarts the channel process. Decoder role uses the same binary.*

Eigener Video-Encoder und -Decoder: **Eingang → Encoder → SRT** (dazu optional RTMP), bedient über eine Weboberfläche auf einem Bildschirm. Dieselbe Software empfängt auch: **SRT → Decoder → DeckLink-Ausgang oder Fenster**.

## Was es kann (Version 0.1)

- **Ein Kanal**, Rolle umschaltbar: *Senden* (Encoder) oder *Empfangen* (Decoder).
- **Eingänge:** DeckLink/UltraStudio (SDI/HDMI, Format wird erkannt, Halbbilder werden aufgelöst) oder Testbild mit 1-kHz-Ton. „Automatisch“ nimmt das erste gefundene DeckLink-Gerät, sonst das Testbild – und sagt das in der Oberfläche.
- **Encoder:** Hardware zuerst – Mac `vtenc_h264_hw`/`vtenc_h265_hw`, Linux NVENC, VA-API, Quick Sync –, sonst x264/x265. Lässt sich ein Hardware-Encoder laden, aber nicht öffnen, nimmt der Kanal von selbst den nächsten. Voreinstellung latenzarm: keine B-Frames, kein Lookahead, GOP 1 s, CBR, SPS/PPS vor jedem Keyframe.
- **Formate:** 720p50 bis 2160p50, H.264 oder H.265. Ein HD-Eingang wird für UHD-Ausgänge mehrthreadig mit Lanczos hochskaliert (am M1 Pro 1080p50 → 2160p50 in Echtzeit).
- **SRT** als Caller („An Ziel senden“) oder Listener („Auf Gegenstelle warten“), Latenz, Passwort (AES), Stream-ID. Eine eingefügte Adresse `srt://host:port?mode=…` wird zerlegt.
- **RTMP** zusätzlich (H.264 + AAC) in einer eigenen Teil-Pipeline: Bricht RTMP ab, versucht es nach 5 s erneut, SRT läuft unberührt weiter.
- **Robust wie das Vorbild:**
  - Ein Signalwächter (`fallbackswitch`) schaltet nach 500 ms ohne Bild auf ein Standbild um (Balken, Schwarz oder Bilddatei, mit Text), Ton auf Stille. Der Ausgang reißt nicht ab, und kommt das Signal zurück, schaltet er zurück.
  - Der Kanal läuft als eigener Prozess. Ein Aufpasser startet ihn neu, wenn er abstürzt, sich 6 s nicht meldet oder 8 s keine Daten mehr ausgibt. Die Pause zwischen den Versuchen wächst von 1 s bis höchstens 10 s.
  - Endet der Aufpasser, beendet sich auch der Kanal. Es bleiben keine verwaisten Prozesse zurück.
- **Live-Status:** Vorschaubild, Signal und Eingangsformat, Bitrate, SRT-RTT, Verluste und Wiederholungen, Encoder (Hardware/Software), Bilder/s, Laufzeit, Neustarts, Ereignisliste. Mit „Signalverlust testen“ lässt sich das Standbild ohne Kabelziehen ausprobieren.
- **Keine Pflichtfelder:** Ein leerer Start ergibt einen SRT-Listener auf Port 9000 mit Testbild. Fehlt etwas, erscheint ein Hinweis, gesperrt wird nichts.

## Gemessen (MacBook Pro M1 Pro, 05.11.2026)

`lz-streamer latenz` misst ohne Kamera. Encoder und Decoder laufen im selben Prozess, über SRT auf 127.0.0.1. Das Testbild springt von Schwarz auf Weiß, und die Zeit läuft, bis der Decoder-Ausgang hell wird. Nicht enthalten sind Kamera, Capture, Netz und Bildschirm.

| Weg | SRT-Latenz | Median (7 Runden) |
|---|---|---|
| H.264 VideoToolbox → avdec_h264 | 120 ms | 227 ms |
| H.264 VideoToolbox → avdec_h264 | 40 ms | 163 ms |
| H.265 VideoToolbox → avdec_h265 | 120 ms | 220 ms |
| H.265 VideoToolbox → vtdec_hw (Voreinstellung für H.265) | 120 ms | 549 ms |

`latenz` schlüsselt zusätzlich nach Stationen auf (Quelle, Encode, Parser+Mux, SRT+Demux+Decode, Ausgabe). Mit SRT 40 ms, Demux 0 und Reserve 20 sind es 134–155 ms; der größte vermeidbare Posten ist mit 37–51 ms die Wartezeit im `mpegtsmux`. Vergleich mit Lynx, AJA, Haivision und Matrox, Hardware-Empfehlung und Latenz-Rechenmodell: **[docs/vergleich-hardware-latenz.md](docs/vergleich-hardware-latenz.md)**.

Unterwegs gefunden und behoben:

- `srtsrc` meldet die SRT-Latenz als Pipeline-Latenz, obwohl die Pakete erst nach Ablauf dieses Puffers ankommen. Die Latenz kam dadurch am Ausgang doppelt dazu. Ausgeglichen wird das mit `ts-offset` und einer festen Pipeline-Latenz: 456 statt 227 ms.
- `vtdec_hw` hält Bilder zurück. Bei H.264 bringt das rund +100 ms, bei H.265 rund +330 ms. Für H.264 dekodiert deshalb libav im Slice-Threading.
- H.265 in UHD schafft libav am Mac nicht in Echtzeit (8–12 fps bei 2160p25). Für H.265 bleibt darum die Hardware die Voreinstellung.
- Dynamisch angehängte Decoder-Senken brauchen `async=false`. Sonst blockiert die Ton-Senke im Preroll, und der Decoder steht still (Deadlock).

## Starten

```bash
# Abhängigkeiten (macOS)
brew install rust gstreamer          # enthält gst-plugins-base/good/bad/ugly, libav, rs (fallbackswitch), srt
# Linux (Debian/Ubuntu)
sudo apt install cargo libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev gstreamer1.0-plugins-{base,good,bad,ugly} \
  gstreamer1.0-libav gstreamer1.0-plugins-rs   # fallbackswitch steckt in gst-plugins-rs

cargo build --release
./target/release/lz-streamer info          # was ist da: Encoder, DeckLink, fehlende Bausteine
./target/release/lz-streamer               # Oberfläche auf http://127.0.0.1:4200
./target/release/lz-streamer serve --bind 0.0.0.0 --autostart   # im Netz erreichbar, Kanal sofort an
./target/release/lz-streamer latenz [--h265] [--srt-latency 40] [--decoder avdec_h265] [--demux 0] [--reserve 20]
```

- **Port 4200.** 4181–4198 belegt die AV-Suite, 4199 belegt `lz-nvx-toolbox`.
- Die Konfiguration liegt unter `~/Library/Application Support/lz-streamer/config.json` (Linux: `~/.config/lz-streamer/`). Dort stehen auch SRT-Passwort und RTMP-Key im Klartext; sie gelangen nie ins Repo.
- Encoder und Decoder auf einem Rechner: zwei Instanzen mit `--port` und `--config` trennen.

**DeckLink/UltraStudio am Mac:** Blackmagic Desktop Video installieren und die Treibererweiterung freigeben (Systemeinstellungen → Anmeldeobjekte & Erweiterungen). Ohne angestecktes Gerät meldet `lz-streamer info` keine DeckLink-Geräte, und „Automatisch“ nimmt das Testbild. Nach dem Anstecken in der Oberfläche neu laden (`POST /api/devices/refresh`) oder den Dienst neu starten. Ein UltraStudio Recorder 3G nimmt höchstens 1080p (3G-SDI). Für 12G/2160p braucht es z. B. ein UltraStudio 4K Mini oder eine DeckLink-4K/8K-Karte; der Weg über `decklinkvideosrc mode=auto` bleibt derselbe.

## Tests

```bash
cargo test
```

- **30 Unit-Tests** prüfen Konfiguration, Zielzerlegung, Encoder-Auswahl und Voreinstellungen, Pipeline-Bau, SRT-Statistik und das Kindprozess-Protokoll.
- **9 Integrationstests** laufen über echte SRT-Verbindungen auf 127.0.0.1:
  - Listener → Caller, Caller → Listener
  - H.265 sowie x264 in Software
  - HD-Testbild auf UHD hochskaliert, als H.265 übertragen
  - Signalverlust: Das Standbild übernimmt, der Decoder bekommt weiter Bilder
  - Latenzmessung
  - RTMP-Abbruch reißt SRT nicht mit (braucht `ffmpeg` als RTMP-Gegenstelle)
  - Kindprozess meldet Status und beendet sich ohne Elternprozess

## Aufbau

```
lz-streamer serve ── axum (REST + Weboberfläche, eingebettet) ── Aufpasser
                                                                   │ startet/überwacht
                                                                   ▼
                                     lz-streamer channel  (ein Prozess je Kanal, JSON-Zeilen über stdin/stdout)
   Eingang ─► valve ─► fallbackswitch ◄─ Standbild           ┌► mpegtsmux ─► srtsink (Caller/Listener)
                         └► NV12 ─► tee ─► Encoder ─► Parser ┤
                                     └► Vorschau-JPEG        └► Abgriff ─► eigene RTMP-Pipeline (flvmux ─► rtmp2sink)
   Decoder: srtsrc ─► tsdemux ─► Parser ─► Decoder ─► DeckLink/Fenster/keine  (+ Ton, + Vorschau)
```

`src/pipeline.rs` baut die Pipelines als reine Funktionen (prüfbar ohne GStreamer), `src/channel.rs` betreibt sie, `src/supervisor.rs` ist der Aufpasser, `src/latency.rs` die Messung.

### API

| Weg | Zweck |
|---|---|
| `GET /api/state` | Konfiguration, Hinweise, Fähigkeiten, Geräte, Laufzeitstatus, Ereignisse |
| `GET/PUT /api/config` | Konfiguration lesen/schreiben; Teil-JSON genügt, der Rest bekommt Vorgaben. Bei laufendem Kanal folgt ein Neustart mit den neuen Werten |
| `POST /api/start` · `/api/stop` | Kanal ein/aus |
| `POST /api/signal {"lost":true}` | Signalverlust nachstellen |
| `POST /api/keyframe` | Keyframe erzwingen |
| `POST /api/devices/refresh` | DeckLink-Geräte neu suchen |
| `GET /api/preview.jpg` | Vorschaubild (2 fps) |

## Bezug zum Vorbild

Vorbild ist ein Software-Encoder auf Server-Hardware aus dem Broadcast-Umfeld (Haivision KB). Übernommen ist nur das Betriebsmuster, nicht Code, Texte oder Gestaltung:

- ein Prozess je Kanal unter einem Aufpasser mit Neustart
- ein Standby-Eingang, der bei Signalverlust einspringt, ohne dass der Ausgang abreißt
- SRT als Hauptweg, Caller und Listener

Anders als dort:

- Hardware-Encoder der jeweiligen Plattform statt Intel-only
- alles auf einer Seite, ohne Pflichtfelder
- Encoder und Decoder in einer Software
- offen gegenüber jeder Standard-libsrt-Gegenstelle

## Was (noch) nicht geht – Fahrplan

Reihenfolge nach `haivision-kb-analyse/docs/nachbau-konzept.md` §5–§7:

1. **Broadcast-TS:** CBR-Mux mit T-STD-Prüfung (TB/MB/EB), PCR-Pacing, PCR ≤ 40 ms, PSI ≤ 100 ms, TSDuck als Abnahme. Heute ist der Mux nur `mpegtsmux` ohne Nullpaket-CBR.
2. **A/V-Sync über Tage:** DeckLink-Uhr als Pipeline-Uhr, Ton-PTS aus dem Samplezähler, adaptives Resampling, 72-h-Dauertest.
3. **Ancillary:** CEA-608/708 aus VANC nach SEI, ST 2038, **SCTE-104 → SCTE-35** mit IDR am Splice-Punkt.
4. **Redundanz:** SRT Socket Groups (broadcast/backup) über zwei Netze, FEC/RIST, SMPTE 2022-7 – Voraussetzung für Ziele wie 120 ms interkontinental (§5.8).
5. **Mehrkanal**, ABR-Leiter, HLS/CMAF.
6. **4:2:2 10-bit** (NVENC Blackwell / VideoToolbox), HDR-Kennzeichnung (PQ/HLG).
7. **Decoder:**
   - Latenzarmer UHD-HEVC-Decode am Mac, denn `vtdec_hw` puffert, siehe Messung
   - Genlock und Referenz
   - Standbild bei SRT-Abriss
8. **Hochskalieren auf der GPU** statt auf der CPU (Metal/VideoToolbox), Seitenverhältnis- und Beschnittmodi.
9. **Anmeldung** für die Oberfläche und Anbindung an `lz-camera-bridge` (Geräteseite, NMOS).

## Offene Punkte

- **Echte Hardware ungeprüft:** Am UltraStudio Recorder 3G lief noch kein Test, das Gerät war beim Bau nicht angesteckt. Das betrifft den DeckLink-Ausgang des Decoders, NVENC und VA-API unter Linux.
- **Umschaltlücke:** Zwischen Signalverlust und Standbild liegen `standby.timeout_ms` ohne neue Bilder (Voreinstellung 500 ms).
- **UHD50 H.265** ist am M1 Pro Grenzlast: 4K-HEVC-Encode schafft dort ≈ 52 fps.
