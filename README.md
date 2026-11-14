# lz-streamer

A single-channel SRT video encoder/decoder in Rust + GStreamer with a one-screen web UI. Hardware encoders first (VideoToolbox, NVENC, VA-API/QSV), x264/x265 as fallback, standby picture on signal loss without dropping the output, and a supervisor that restarts the channel process. Decoder role uses the same binary.*

Eigener Video-Encoder und -Decoder: **Eingang → Encoder → SRT** (dazu optional RTMP), bedient über eine Weboberfläche auf einem Bildschirm. Dieselbe Software empfängt auch: **SRT → Decoder → DeckLink-Ausgang oder Fenster**.

## Was es kann (Version 0.1)

- **Ein Kanal**, Rolle umschaltbar: *Senden* (Encoder) oder *Empfangen* (Decoder).
- **Eingänge:** DeckLink/UltraStudio (SDI/HDMI, Format wird erkannt, Halbbilder werden aufgelöst), USB-Video (UVC, z. B. AJA U-TAP; höchstens 1080p60, Ton über den Standard-Toneingang, kein VANC), Videodatei oder Testbild mit 1-kHz-Ton. Dazu **Netz-Eingänge:** MPEG-TS über SRT (Caller, Listener, Rendezvous; Passwort, Stream-ID), UDP und RTP (auch Multicast) sowie RTMP von einem Server abholen. Eine eingefügte Adresse (`srt://…`, `udp://@239.1.2.3:1234`, `rtp://…`, `rtmp://…`) wird zerlegt und stellt die Eingangsart. „Neu kodieren“ dekodiert und läuft durch die ganze Kette (Standbild, Tonmatrix, Untertitel); fällt die Quelle aus, kommt das Standbild, kehrt sie zurück, verbindet der Eingang selbst neu. „Durchreichen“ gibt den Transportstrom ohne Neukodierung auf alle TS-Ausgänge (Gateway), ohne Standbild und ohne RTMP-Ausgang. „Automatisch“ nimmt das erste gefundene DeckLink-Gerät, sonst das Testbild – und sagt das in der Oberfläche.
- **Encoder:** Hardware zuerst – Mac `vtenc_h264_hw`/`vtenc_h265_hw`, Linux NVENC, VA-API, Quick Sync –, sonst x264/x265. Lässt sich ein Hardware-Encoder laden, aber nicht öffnen, nimmt der Kanal von selbst den nächsten. Voreinstellung latenzarm: keine B-Frames, kein Lookahead, GOP 1 s, CBR, SPS/PPS vor jedem Keyframe.
- **Formate:** 720p50 bis 2160p50, H.264 oder H.265. Ein HD-Eingang wird für UHD-Ausgänge mehrthreadig mit Lanczos (Hüllkurve 3) hochskaliert (am M1 Pro 1080p50 → 2160p50 in Echtzeit).
- **SRT** als Caller („An Ziel senden“) oder Listener („Auf Gegenstelle warten“), Latenz, Passwort (AES), Stream-ID. Eine eingefügte Adresse `srt://host:port?mode=…` wird zerlegt.
- **RTMP** zusätzlich (H.264 + AAC) in einer eigenen Teil-Pipeline: Bricht RTMP ab, versucht es nach 5 s erneut, SRT läuft unberührt weiter.
- **Robust wie das Vorbild:**
  - Ein Signalwächter (`fallbackswitch`) schaltet nach 500 ms ohne Bild auf ein Standbild um (Balken, Schwarz oder Bilddatei, mit Text), Ton auf Stille. Der Ausgang reißt nicht ab, und kommt das Signal zurück, schaltet er zurück.
  - Der Kanal läuft als eigener Prozess. Ein Aufpasser startet ihn neu, wenn er abstürzt, sich 6 s nicht meldet oder 8 s keine Daten mehr ausgibt. Die Pause zwischen den Versuchen wächst von 1 s bis höchstens 10 s.
  - Endet der Aufpasser, beendet sich auch der Kanal. Es bleiben keine verwaisten Prozesse zurück.
- **Live-Status:** Vorschaubild, Signal und Eingangsformat, Bitrate, SRT-RTT, Verluste und Wiederholungen, Encoder (Hardware/Software), Bilder/s, Laufzeit, Neustarts, Ereignisliste. Mit „Signalverlust testen“ lässt sich das Standbild ohne Kabelziehen ausprobieren.
- **Ton wie im Broadcast:** Matrix vom Eingang (2/8/16 eingebettete SDI-Kanäle) auf bis zu 8 Tonspuren je Ausgang, Mono/Stereo/Downmix, Pegel, Stumm, Sprachkennung (ISO 639) im TS. Der Decoder wählt die Spur nach Sprache oder Nummer.
- **Grafiken (OGraf):** Neben den Untertiteln (Ebene 1) nimmt Ebene 2 beliebige OGraf-Grafiken des Renderers auf, etwa Bauchbinde (`lz-lower-third`), Titel oder Liedtexte (`lz-lyrics`). Bedienung in der Karte „Grafik“, über `POST /api/graphics` oder per OSC an `127.0.0.1:4211`:
  - `/lzs/gfx/show s:grafik [s:JSON]` lädt und blendet ein
  - `/lzs/gfx/next` und `/lzs/gfx/prev` schalten weiter und zurück
  - `/lzs/gfx/update s:JSON` ändert den Inhalt
  - `/lzs/gfx/stop` blendet aus, `/lzs/gfx/clear` entlädt
  - `/lzs/gfx/custom s:id [s:JSON]` löst eine eigene Aktion der Grafik aus

  In Companion geht das mit dem Modul „Generic OSC“. Verbindet sich der Renderer neu, zeigt er die laufende Grafik wieder.
- **Klare Sprache:** Eine zusätzliche Tonspur, in der DeepFilterNet3 Musik, Geräusch und Hall um einen einstellbaren Wert absenkt (Vorgabe −9 dB). Bei erkanntem Gesang wird nicht abgesenkt. Die Spur ist 30 ms verzögert und bleibt über die Zeitstempel lippensynchron; die Hauptspur bleibt unverändert. Messung: [docs/klare-sprache.md](docs/klare-sprache.md).
- **Plugins je Tonspur, VST3 und am Mac Audio Units** (z. B. LZ Defeedback, Apples AU-Effekte): Das Plugin läuft in einem eigenen Hilfsprozess. Stürzt es ab oder antwortet es nicht innerhalb einer halben Blockdauer, geht der Ton unbearbeitet weiter, ohne Lücke und ohne Neustart des Kanals; der Hilfsprozess startet von selbst neu. Die Anzeige je Spur zeigt „aktiv“ oder „Bypass“. Gefunden werden die Plugins in den üblichen VST3-Ordnern und über die AU-Registrierung des Mac. Jedes wird beim ersten Mal in einem eigenen Prozess geprüft. Wer dabei abstürzt oder hängt, kommt auf eine Sperrliste. Das Ergebnis bleibt zwischengespeichert, „Plugins neu suchen“ prüft erneut.
- **Mehrere Ausgänge** aus demselben Strom: weitere SRT-Ziele (auch Rendezvous), TS über UDP/RTP (auch Multicast), dazu RTMP.
- **Aufnahme parallel** zum Stream: TS oder fragmentiertes MP4, Segmente, alle Tonspuren. Mit Untertiteln liegen neben jedem Segment `.srt` und `.vtt`, deren Zeiten aus dem gesendeten 608-Strom stammen. Eine zweite Untertitelsprache liegt auf CC3. Mit Spracherkennung übersetzt Whisper ins Englische, bei Live-Text kommt sie aus einem eigenen Eingabefeld. Der Decoder wählt CC1 oder CC3 ([Messung](docs/untertitel-messung.md)). Aufnahmen-Liste mit Download; älteste löschen, wenn die Platte voll wird.
- **Upload fertiger Segmente** zu WebDAV, S3 oder SFTP, samt `.srt`/`.vtt`: Bandbreite in kbit/s begrenzbar (Token-Bucket für alle Uploads zusammen), Warteschlange auf Platte, nach einem Abbruch neuer Versuch mit wachsender Pause (5 s bis 10 min). WebDAV und S3 laden dann vollständig neu, SFTP setzt per `reput` fort. Auf Wunsch wird lokal gelöscht, sobald alle Ziele die Datei haben. S3 signiert selbst (SigV4, Nutzlast unsigniert) und nimmt höchstens 5 GB je Datei. Passwörter und Secret Keys liegen nur in `upload.json` (Rechte 0600) und kommen nie über API, Oberfläche, Diagnose oder Export zurück.
- **Verwaltung:**
  - Profile (mitgelieferte und eigene)
  - Zeitplan: wöchentlich oder einmalig, mit Profilwahl
  - Alarme mit Schwellen, per Mail (SMTP) mit Entwarnung und Testmail
  - Messwerte für Prometheus (`/metrics`) und Influx (Push)
  - Standbilder hochladen
  - Statistik über 24 h, Bericht als CSV
  - Systemlast mit GPU (Mac über ioreg, NVIDIA über nvidia-smi), Alarme für Bildrate und Formatwechsel am Eingang
  - Diagnose-Paket und Kanal-Export/-Import als Datei
  - Benutzer mit Rollen (Verwalten/Bedienen/Ansehen)
  - HTTPS mit selbst signiertem oder eigenem Zertifikat (PEM), TLS 1.2/1.3
  - Beschnitt
- **Schalter live:** Untertitel, Klare Sprache, Lautheitsregelung samt Profil, Plugin-Umgehung und Grafik lassen sich während der Sendung umschalten, ohne dass der Kanal neu startet. Das geht in der Karte „Schalter“, über `/api/control` oder per OSC, etwa aus Companion.
- **Keine Pflichtfelder:** Ein leerer Start ergibt einen SRT-Listener auf Port 9000 mit Testbild. Fehlt etwas, erscheint ein Hinweis, gesperrt wird nichts.

## Gemessen (MacBook Pro M1 Pro, 05.11.2026)

`lz-streamer latenz` misst ohne Kamera. Encoder und Decoder laufen im selben Prozess, über SRT auf 127.0.0.1. Das Testbild springt von Schwarz auf Weiß, und die Zeit läuft, bis der Decoder-Ausgang hell wird. Nicht enthalten sind Kamera, Capture, Netz und Bildschirm.

| Weg | SRT-Latenz | Median (7 Runden) |
|---|---|---|
| H.264 VideoToolbox → avdec_h264 | 120 ms | 227 ms |
| H.264 VideoToolbox → avdec_h264 | 40 ms | 163 ms |
| H.265 VideoToolbox → avdec_h265 | 120 ms | 220 ms |
| H.265 VideoToolbox → vtdec_hw (Voreinstellung für H.265) | 120 ms | 549 ms |

`latenz` schlüsselt zusätzlich nach Stationen auf (Quelle, Encode, Parser+Mux, SRT+Demux+Decode, Ausgabe). Mit SRT 40 ms, Demux 0 und Reserve 20 sind es 134–155 ms; der größte vermeidbare Posten ist mit 37–51 ms die Wartezeit im `mpegtsmux`. Weitere Dokumente:

| Thema | Dokument |
|---|---|
| Vergleich mit Lynx, AJA, Haivision und Matrox, Hardware-Empfehlung, Latenz-Rechenmodell | [docs/vergleich-hardware-latenz.md](docs/vergleich-hardware-latenz.md) |
| Funktionsparität mit dem Vorbild | [docs/funktionsparitaet-kb.md](docs/funktionsparitaet-kb.md) |
| Messung des Hochskalierens (Zeit, Last, VMAF/PSNR) | [docs/skalierung.md](docs/skalierung.md) |
| Hardware-Skalierer und Plattformen | [docs/recherche-hardware-scaler.md](docs/recherche-hardware-scaler.md) |

Unterwegs gefunden und behoben:

- `srtsrc` meldet die SRT-Latenz als Pipeline-Latenz, obwohl die Pakete erst nach Ablauf dieses Puffers ankommen. Die Latenz kam dadurch am Ausgang doppelt dazu. Ausgeglichen wird das mit `ts-offset` und einer festen Pipeline-Latenz: 456 statt 227 ms.
- `vtdec_hw` hält Bilder zurück. Bei H.264 bringt das rund +100 ms, bei H.265 rund +330 ms. Für H.264 dekodiert deshalb libav im Slice-Threading.
- H.265 in UHD schafft libav am Mac nicht in Echtzeit (8–12 fps bei 2160p25). Für H.265 bleibt darum die Hardware die Voreinstellung.
- Dynamisch angehängte Decoder-Senken brauchen `async=false`. Sonst blockiert die Ton-Senke im Preroll, und der Decoder steht still (Deadlock).

## Herunterladen

Fertige Pakete stehen unter [Releases](../../releases):
- **macOS (Apple Silicon):** `lz-streamer-<version>-macos-arm64.tar.gz`. Voraussetzung: `brew install gstreamer`.
- **Windows (x64):** `lz-streamer-<version>-windows-x64.zip`. Voraussetzung: der GStreamer-1.26-Laufzeit-Installer (MSVC 64 bit, „Complete“).

Beide Pakete bestehen aus einer einzelnen Datei; die Oberfläche ist eingebaut. Hinweise stehen in `LIESMICH.txt` im Paket.

Release bauen: Tag `v<version>` setzen und das Mac-Paket lokal bauen (`cargo build --release`). Das Windows-Paket entsteht auf Zuruf mit `gh workflow run "Release Windows" -f tag=v<version>`; Actions-Minuten kosten im privaten Repo Geld.

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
./target/release/lz-streamer serve --bind 0.0.0.0 --https       # über HTTPS (auch in der Verwaltung einstellbar)
./target/release/lz-streamer latenz [--h265] [--srt-latency 40] [--decoder avdec_h265] [--demux 0] [--reserve 20]
```

- **Port 4200.** 4181–4198 belegt die AV-Suite, 4199 belegt `lz-nvx-toolbox`.
- Die Konfiguration liegt unter `~/Library/Application Support/lz-streamer/config.json` (Linux: `~/.config/lz-streamer/`). Dort stehen auch SRT-Passwort und RTMP-Key im Klartext; sie gelangen nie ins Repo.
- Encoder und Decoder auf einem Rechner: zwei Instanzen mit `--port` und `--config` trennen.
- **HTTPS:** Beim ersten Start mit HTTPS entsteht ein selbst signiertes Zertifikat (CN = Rechnername, SANs = Rechnername, `localhost`, eigene IPv4-Adressen, 790 Tage gültig) unter `tls/` neben der Konfiguration. Fehlt später eine Adresse, wird es neu erzeugt. Ein eigenes Zertifikat (PEM mit Kette und Schlüssel) lässt sich in der Verwaltung einspielen und gilt sofort. HTTPS und Klartext teilen sich den Port: Klartext von außen bekommt eine Weiterleitung auf `https://`, Klartext von 127.0.0.1 wird weiter bedient (Grafik-Renderer, lokale Werkzeuge). Die Prüfung „nur lokal“ sieht die echte Gegenstelle.

**DeckLink/UltraStudio am Mac:** Blackmagic Desktop Video installieren und die Treibererweiterung freigeben (Systemeinstellungen → Anmeldeobjekte & Erweiterungen). Ohne angestecktes Gerät meldet `lz-streamer info` keine DeckLink-Geräte, und „Automatisch“ nimmt das Testbild. Nach dem Anstecken in der Oberfläche neu laden (`POST /api/devices/refresh`) oder den Dienst neu starten. Ein UltraStudio Recorder 3G nimmt höchstens 1080p (3G-SDI). Für 12G/2160p braucht es z. B. ein UltraStudio 4K Mini oder eine DeckLink-4K/8K-Karte; der Weg über `decklinkvideosrc mode=auto` bleibt derselbe.

## Tests

```bash
cargo test
```

- **51 Unit-Tests** prüfen Konfiguration, Zielzerlegung, Encoder-Auswahl und Voreinstellungen, Pipeline-Bau, SRT-Statistik und das Kindprozess-Protokoll.
- **4 API-Tests** (`tests/api.rs`): Anmeldung und Rollen, Profile, Standbilder, Zeitplan, Alarme, Aufnahmen, Bericht.
- **3 Überwachungstests** (`tests/alarm_mail.rs`): Ein lokaler Mini-SMTP-Server empfängt die Alarm-Mail bei nachgestelltem Signalverlust und die Entwarnung. Passwort und Token tauchen in keiner Antwort auf. `/metrics` prüft Anmeldung, HTTP-Basic und Freigabe.
- **2 HTTPS-Tests** (`tests/https.rs`): `/api/state` über TLS 1.3 und 1.2 (rustls-Client), Zertifikat für IP und Namen, eigenes Zertifikat gilt ohne Neustart, falscher Schlüssel wird abgelehnt, Renderer von einer Netzadresse abgelehnt, Klartext von außen umgeleitet.
- **5 Upload-Tests** (`tests/upload.rs`): WebDAV-Ersatz (axum) mit Basic-Auth, Abbruch mitten im PUT und Neuversuch, gemessene Drosselung (4000 kbit/s eingestellt, ≈ 4040 gemessen); Warteschlange übersteht Neustart; S3-PUT mit vom Ersatz nachgerechneter Signatur; SFTP gegen eigenen `sshd` (Drosselung, `reput`); 4 MB Upload neben laufendem SRT-Durchlauf ohne Verluste, danach folgt das beendete Segment automatisch. SigV4 zusätzlich gegen zwei Beispielvektoren aus der AWS-Dokumentation.
- **13 Integrationstests** laufen über echte SRT-Verbindungen auf 127.0.0.1:
  - Listener → Caller, Caller → Listener
  - H.265 sowie x264 in Software
  - HD-Testbild auf UHD hochskaliert, als H.265 übertragen
  - Signalverlust: Das Standbild übernimmt, der Decoder bekommt weiter Bilder
  - Latenzmessung
  - RTMP-Abbruch reißt SRT nicht mit (braucht `ffmpeg` als RTMP-Gegenstelle)
  - Kindprozess meldet Status und beendet sich ohne Elternprozess
  - drei Tonspuren mit Sprache (de/en/fr) kommen am Decoder an, Sprachwahl greift
  - Aufnahme als TS und MP4 mit allen Spuren (Prüfung mit ffprobe)
  - zweiter SRT-Ausgang und UDP-Ausgang
  - Netz-Eingänge: SRT (H.265) neu kodiert auf H.264 mit Standbild bei Ausfall und Wiederverbinden, SRT durchgereicht ohne Encoder, UDP neu kodiert, RTP durchgereicht, RTMP von ffmpeg abgeholt

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
| `GET/POST /api/presets`, `POST /api/presets/{name}/load`, `DELETE …` | Profile |
| `GET/PUT /api/settings` | Zeitplan, Alarmschwellen, Aufnahmen automatisch löschen, Mail, Messwerte; Passwort und Token kommen leer zurück |
| `GET /api/alarms` · `/api/stats?minutes=` · `/api/system` · `/api/report.csv` | Alarme, Verlauf, Systemlast, Bericht |
| `POST /api/mail/test` | Testmail mit den gespeicherten Mail-Einstellungen |
| `GET /metrics` | Messwerte im Prometheus-Textformat (siehe unten) |
| `GET /api/diagnose.json` · `GET /api/config/export` | Diagnose-Paket (Passwörter geschwärzt) · Kanal als Datei (vollständig; einspielen mit `PUT /api/config`) |
| `GET/POST /api/standby`, `GET/DELETE /api/standby/{name}`, `POST …/use` | Standbilder |
| `GET /api/recordings`, `GET/DELETE /api/recordings/{name}`, `POST …/upload` | Aufnahmen; von Hand hochladen |
| `GET/PUT /api/uploads`, `POST /api/uploads/retry` · `/clear` | Upload-Ziele (ohne Geheimnisse; leeres Feld = unverändert, `clear_secret: true` löscht) · sofort neu versuchen · Warteschlange leeren. Status steht in `/api/state` unter `uploads` |
| `GET /api/plugins` · `POST /api/plugins/scan` | geprüfte Plugins und Sperrliste · neu prüfen |
| `GET/POST /api/control` | schaltbare Eigenschaften lesen bzw. live umstellen, ohne Neustart: `{"path":"captions.enabled","value":false}`; Pfade siehe `src/control.rs`, benannt für NMOS IS-12. Per OSC: `/lzs/set/<pfad mit />` mit Wert, z. B. `/lzs/set/audio/tracks/0/plugins/0/bypass T` |
| `GET/POST /api/graphics` | Zustand der Grafik-Ebenen; Befehl `{"action":"show","graphicId":"lz-lower-third","data":{…}}` (`load`, `show`, `play`, `next`, `prev`, `update`, `stop`, `clear`, `custom`) |
| `GET/POST /api/tls`, `POST /api/tls/self-signed` | Zertifikat ansehen (ohne Schlüssel) · eigenes einspielen `{"cert":"PEM","key":"PEM"}` · selbst signiertes neu erzeugen |
| `POST /api/login` · `/api/logout`, `GET /api/me`, `GET/POST /api/users`, `DELETE /api/users/{name}` | Anmeldung und Benutzer; ohne Benutzer bleibt die Oberfläche offen, der erste wird Administrator |

### Überwachung: Prometheus, Influx, Mail

- **`GET /metrics`** liefert alle Werte im Prometheus-Textformat, Präfix `lzs_`. Dazu gehören Bitrate, SRT-RTT, verlorene, wiederholte und verworfene Pakete, Lautheit und True Peak je Spur (Labels `track`, `name`, `language`), CPU, GPU, Speicher, freier Platz, Bilder/s (gemessen und eingestellt), Neustarts, Signal, Standbild und aktive Alarme. Ohne angelegte Benutzer ist der Weg offen. Mit Benutzern braucht er eine Sitzung oder HTTP-Basic mit Name und Passwort aus der Benutzerliste; ohne Anmeldung nur, wenn „`/metrics` ohne Anmeldung abrufbar“ eingeschaltet ist.

  ```yaml
  scrape_configs:
    - job_name: lz-streamer
      static_configs: [{ targets: ["streamer.local:4200"] }]
      basic_auth: { username: prometheus, password: "…" }
  ```
- **Influx:** Mit einer Schreibadresse (z. B. `http://influx:8086/api/v2/write?org=technik&bucket=streamer`) schickt lz-streamer im eingestellten Takt Influx-Line-Protocol, Messung `lz_streamer`, Tag `host`. Das Token geht als `Authorization: Token …` mit. Ein Ausfall erscheint einmal im Ereignisprotokoll, die Rückkehr ebenso.
- **Alarme per Mail:** SMTP mit STARTTLS (587), TLS (465) oder unverschlüsselt im eigenen Netz. Zur Wahl stehen „nur Fehler“ und eine Entwarnung, wenn ein Alarm erledigt ist. Jeder Alarm kommt einmal bei Beginn; alle neuen Alarme eines Prüfdurchlaufs stehen in einer Mail. „Testmail senden“ prüft die Einstellungen sofort.
- **Zugangsdaten** für Mail und Influx stehen nur in `verwaltung.json` im Datenverzeichnis. `GET /api/settings`, das Diagnose-Paket und die Oberfläche geben Passwort und Token nie zurück. Ein leeres Feld beim Speichern lässt den gespeicherten Wert stehen.

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
- **H.265 mit `vtdec_hw`** hält rund 20 Bilder zurück (450 ms bei 50p, 826 ms bei 25p). Für H.265 bis 1080p ist `--decoder avdec_h265` die latenzarme Wahl (220 ms).
- Offene Paritätspunkte mit Begründung: [docs/funktionsparitaet-kb.md](docs/funktionsparitaet-kb.md).
- **Plugins mit eigener Latenz:** Beim Wechsel zwischen Bypass und Plugin verschiebt sich der Ton um die Plugin-Latenz. LZ Defeedback meldet 0 Samples und ist davon nicht betroffen. Geprüft ist nur LZ Defeedback am Mac.
- **UHD50 H.265** ist am M1 Pro Grenzlast: 4K-HEVC-Encode schafft dort ≈ 52 fps.
