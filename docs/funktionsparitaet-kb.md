# Funktionsparität mit dem Vorbild (Haivision KB 5.6)

Stand: 05.11.2026, lz-streamer nach PR #2.

Grundlage:
- UI-Rundgang der KB (46 Screenshots und `kb-ui/bericht.md` im Analyse-Scratchpad)
- `haivision-kb-analyse/docs/software-analyse.md`

Hier stehen nur Funktionen, beschrieben in eigenen Worten. Texte, Logos und Assets des Vorbilds sind nicht übernommen.

**Status:**
- **fertig:** in lz-streamer vorhanden und getestet
- **Teil:** Kern vorhanden, aber schmaler als beim Vorbild
- **fehlt:** noch nicht gebaut
- **entfällt:** gehört beim Mac oder Linux-PC zum Betriebssystem oder ist ohne Lizenzmodell sinnlos

**Aufwand:** S < 1 Tag · M 1–3 Tage · L 1–2 Wochen · XL > 2 Wochen.

## Zahlen

| Status | Anzahl |
|---|---|
| fertig | 37 |
| Teil | 14 |
| fehlt | 8 |
| entfällt | 5 |
| **gesamt** | **64** |

Ohne die entfallenden Punkte sind 37 von 59 Funktionen fertig (63 %), dazu 14 in Teilen. Die 8 fehlenden sind fast alle Broadcast-Breite oder Mehrkanal; sie stehen in §6 mit Begründung.

## 1. Kanäle und Bedienung

| # | Funktion im Vorbild | Status | Umsetzung / was fehlt | Aufwand |
|---|---|---|---|---|
| 1 | Kanalübersicht mit Kachel und Start/Stopp | fertig | Startseite mit Vorschau, Ampel, Start/Stopp | – |
| 2 | Mehrere Kanäle parallel | **fehlt** | ein Kanal je Instanz; zweite Instanz per `--port/--config` | L |
| 3 | Assistent „neuer Kanal“ in 4 Schritten | fertig (anders) | ohne Pflichtfelder: Eingang wird erkannt, Ziel eintragen, Start; Profile als Vorlagen | – |
| 4 | Kanal exportieren, importieren, duplizieren | Teil | Profile sichern/laden, `GET/PUT /api/config` als Export/Import; kein Datei-Dialog | S |
| 5 | Kanal-Vorlagen (Codec × Auflösung × AAC) | fertig | 5 mitgelieferte Profile + eigene | – |
| 6 | Kanal-Statistik-Reiter | fertig | Kennzahlen live, Verlauf 24 h, Diagramm | – |
| 7 | Kanal-Ereignisplan | fertig | Zeitplan (wöchentlich/einmalig, über Mitternacht, mit Profil) | – |
| 8 | Meldungsprotokoll je Kanal | fertig | Ereignisliste, CSV-Bericht | – |
| 9 | Aufpasser startet abgestürzte Kanäle neu | fertig | eigener Prozess je Kanal, Neustart mit Pause, Stillstandserkennung | – |
| 10 | Vorschau-Bild in der Oberfläche | fertig | JPEG 2 fps, auch im Decoder | – |
| 11 | REST-API | fertig | siehe README | – |

## 2. Eingang

| # | Funktion | Status | Umsetzung / was fehlt | Aufwand |
|---|---|---|---|---|
| 12 | Baseband SDI/HDMI (DeckLink), Format automatisch | fertig | an echter Hardware noch ungeprüft | – |
| 13 | Netz-Eingänge: TS über SRT/UDP/RTP, RTMP (Umkodieren) | **fehlt** | Decoder-Rolle empfängt SRT, kodiert aber nicht neu | M |
| 14 | Eingebetteter Ton, Pegel in dB | fertig | 2/8/16 Kanäle, Pegel je Spur | – |
| 15 | Tonkanal-Zuordnung (Modus, L/R, Sprache) | fertig (mehr) | freie Matrix, Mono/Stereo/Downmix, bis 8 Spuren, ISO 639 | – |
| 16 | Standbild bei Signalverlust ein/aus | fertig | Balken, Schwarz oder eigenes Bild, Umschaltzeit einstellbar | – |
| 17 | Hochwertiges Deinterlacing | Teil | `deinterlace` automatisch, nicht wählbar; bwdif-Klasse fehlt | M |
| 18 | Beschnitt (Cropping) | fertig | oben/unten/links/rechts in Pixeln | – |
| 19 | Zeitstempel „Systemzeit“ / „ab null“ | Teil | immer Systemuhr; Wahl ohne Nutzen für SRT, DeckLink-Uhr als Pipeline-Uhr fehlt | M |
| 20 | Untertitel CEA-608/708 Kanal 1–4 | **fehlt** | siehe §6 | M |
| 21 | Freie Zusatzparameter | Teil | Encoder-Wahl, GOP, Decoder-Puffer; keine freie Tabelle | S |

## 3. Video- und Audio-Encoder

| # | Funktion | Status | Umsetzung / was fehlt | Aufwand |
|---|---|---|---|---|
| 22 | H.264/HEVC jeweils Software oder Hardware | fertig | VideoToolbox, NVENC, VA-API, QSV, x264/x265, automatische Rückfallebene | – |
| 23 | Bitrate | fertig | CBR | – |
| 24 | Ausgabeauflösungen inkl. eigener | Teil | Liste 720p–2160p in der Oberfläche, beliebige über API; kleine Stufen (≤ 540p) nicht in der Liste | S |
| 25 | Bildraten-Klassen | fertig | frei einstellbar 1–120 fps | – |
| 26 | Bildstruktur progressiv/interlaced | Teil | nur progressiv | M |
| 27 | Framing IP/IBP/IBBP/IBBBP | Teil | nur IP, bewusst für Latenz; B-Frames wählbar machen | S |
| 28 | Profil, Level, Entropie, Performance-Stufe | Teil | Encoder-Vorgaben latenzarm, nicht einzeln wählbar | S |
| 29 | GOP-Modus und -Dauer | fertig | Dauer in ms | – |
| 30 | Mehrere Video-Encoder je Kanal (ABR-Leiter) | **fehlt** | siehe §6 | L |
| 31 | AAC mit Sprache und Bitrate | fertig | je Spur | – |
| 32 | Hochskalieren | fertig (mehr) | HD → UHD in Echtzeit, Lanczos e3; gemessen in `skalierung.md` | – |

## 4. Ausgänge

| # | Funktion | Status | Umsetzung / was fehlt | Aufwand |
|---|---|---|---|---|
| 33 | RTMP (Social Media) | fertig | eigene Teil-Pipeline, Abbruch reißt SRT nicht mit | – |
| 34 | SRT Caller/Listener | fertig | – | – |
| 35 | SRT Rendezvous | fertig | neu in PR #2 | – |
| 36 | SRT Passwort, Latenz, Stream-ID | fertig | – | – |
| 37 | SRT Overhead-Bandbreite | Teil | libsrt-Vorgabe, nicht einstellbar | S |
| 38 | TS über UDP/RTP (auch Multicast) | fertig | als weitere Ausgänge | – |
| 39 | Mehrere Ausgänge je Kanal | fertig | beliebig viele SRT/UDP/RTP + RTMP | – |
| 40 | HLS/DASH/CMAF | **fehlt** | siehe §6 | M |
| 41 | Aufnahme (Archiv) | fertig | parallel, TS oder fragmentiertes MP4, Segmente, alle Tonspuren | – |
| 42 | Aufnahmen-Seite (Liste, Download) | fertig | Liste, Download, Löschen | – |

## 5. Verwaltung

| # | Funktion | Status | Umsetzung / was fehlt | Aufwand |
|---|---|---|---|---|
| 43 | Systemaktivität (CPU, Speicher, Platte) | Teil | CPU, Speicher, freier Platz, Laufzeit; GPU-Last fehlt | S |
| 44 | Lizenzen | entfällt | kein Lizenzmodell | – |
| 45 | Berichte, Log-Download | Teil | CSV-Bericht (Verlauf + Ereignisse); kein Diagnose-Log-Paket | S |
| 46 | Alarmschwellen CPU/GPU/Speicher/Platte | Teil | CPU, Speicher, Platte; GPU fehlt | S |
| 47 | Laufzeit-Alarme (Signalverlust, Formatwechsel, Bildrate …) | Teil | Signal, Gegenstelle, Bitrate, Verluste, Neustarts; Formatwechsel und Bildraten-Alarm fehlen | S |
| 48 | Alarm-Benachrichtigung in der Oberfläche | fertig | Banner und Liste, Ereignis bei Beginn/Ende | – |
| 49 | Benachrichtigung per E-Mail (SMTP) | fertig | STARTTLS/TLS, Anmeldung, nur Fehler oder alle, Entwarnung, Testmail; Passwort nur in `verwaltung.json`. Test: `tests/alarm_mail.rs` mit lokalem Mini-SMTP-Server | – |
| 50 | Aufnahmeordner | fertig | einstellbar | – |
| 51 | Älteste Aufnahmen löschen, wenn die Platte voll ist | fertig | neu in PR #2 | – |
| 52 | Bandbreitenlimit und Upload-Ziele für Aufnahmen | **fehlt** | siehe §6 | M |
| 53 | Netzwerk (Hostname, DHCP, DNS, NTP, Bonding) | entfällt | Betriebssystem | – |
| 54 | WLAN-Zugangspunkt | entfällt | Betriebssystem | – |
| 55 | Standbilder verwalten (hochladen, wählen) | fertig | PNG/JPEG, Vorschau, „Verwenden“ | – |
| 56 | HTTPS-Zertifikat | fertig | selbst signiert beim ersten Start (Rechnername, eigene IPv4, localhost) oder eigenes PEM, TLS 1.2/1.3 über rustls; Klartext von außen wird umgeleitet | – |
| 57 | Software-Update hochladen | entfällt | Release per v-Tag und Paketverwaltung | – |
| 58 | Benutzer mit Rollen (Verwalten/Bedienen/Ansehen) | fertig | Argon2, Sitzungs-Cookie, Rollenprüfung je Aufruf | – |
| 59 | Info-Dialog (Version) | fertig | `version` in `/api/state` | – |

## 6. Aus der Software-Analyse (nicht in der Oberfläche sichtbar)

| # | Funktion | Status | Umsetzung / was fehlt | Aufwand |
|---|---|---|---|---|
| 60 | SCTE-35/104, Werbe-Cue-Punkte | **fehlt** | Übersetzer SCTE-104 → 35 mit IDR am Splice-Punkt | L |
| 61 | Ancillary: AFD, Timecode, Durchreichen | **fehlt** | VANC-Parser aus lz-scopes übernehmen, ST 2038 | M |
| 62 | Diskontinuität als Ereignis durch die Pipeline | Teil | Ereignis im Protokoll; kein In-band-Signal für HLS/RTMP | M |
| 63 | Cloud-Anbindung (Geräteregistrierung, CDN-Logins) | entfällt bewusst | Fernzugriff per Tunnel; Geräteseite in lz-camera-bridge | – |
| 64 | Metriken in Zeitreihen-DB | fertig | `/metrics` im Prometheus-Format (Bitrate, SRT-RTT, Verluste, Wiederholungen, Lautheit je Spur, CPU/GPU, Bilder/s, Neustarts, Signal, Alarme); Influx-Line-Protocol per HTTP-Push | – |

## Was offen bleibt und warum

Gezählt sind die 8 fehlenden Funktionen und die drei Teilfunktionen mit dem größten Abstand. Für den Abschluss dieser Runde war entscheidend, dass jeder umgesetzte Punkt mit einem Test belegt ist. Alles, was hier steht, hätte in dieser Runde keinen sauberen Test bekommen.

| Punkt | Warum noch nicht |
|---|---|
| Mehrkanal (#2), ABR-Leiter (#30) | Umbau von Aufpasser, Statusmodell und Oberfläche auf Kanallisten; eigener PR |
| Netz-Eingänge mit Umkodieren (#13) | eigener Eingangszweig (srtsrc/udpsrc → decodebin → Umschalter); braucht Tests mit Formatwechsel |
| CEA-608/708 (#20), SCTE (#60), Ancillary (#61) | braucht VANC aus dem DeckLink-Eingang; ohne angestecktes Gerät nicht prüfbar |
| HLS/CMAF (#40) | `hlscmafsink` vorhanden; Speicherort, Auslieferung und Aufräumen fehlen noch |
| Upload-Ziele (#52) | klein; nach Mehrkanal sinnvoller |
| Interlaced-Ausgabe (#26), Deinterlacing-Wahl (#17) | erst mit 1080i-Quelle an echter Hardware sinnvoll zu messen |

## Neu in PR #2

Jede Zeile hat einen eigenen Test:

| Funktion | Test |
|---|---|
| Tonspuren mit Sprachkennung über SRT | `tests/roundtrip.rs` `multilingual_audio_tracks_arrive_with_language_codes` (3 Spuren de/en/fr, Sprachwahl „eng“) |
| Aufnahme parallel | `recording_ts_…`, `recording_mp4_…` (ffprobe: Video + 3 Tonspuren, 2/2/1 Kanäle, deu/eng/fra) |
| Weitere Ausgänge | `extra_outputs_srt_and_udp` (zweiter SRT-Listener liefert Bilder, UDP liefert TS-Pakete) |
| Anmeldung und Rollen | `tests/api.rs` `login_and_roles` |
| Profile | `presets_save_and_load` |
| Standbilder | `standby_images` |
| Zeitplan, Alarme, Aufnahmen-API, CSV-Bericht | `schedule_alarms_report_recordings` |
| Zeitfenster-, Alarm-, Rollen- und Matrixlogik | Unit-Tests in `admin.rs`, `audio.rs`, `ops.rs` |
| Rendezvous, Beschnitt | Unit-Tests in `config.rs`, `pipeline.rs` |
