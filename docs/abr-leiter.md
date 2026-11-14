# ABR-Leiter und HLS

## Aufbau

```text
            ┌► Haupt-Encoder (venc) ─► Parser ─► tee ve ─► SRT/RTMP/Aufnahme
Rohbild vt ─┤                                        └► hlssink3 v.m3u8
            ├► videoscale ─► venc_r1 ─► Parser ─► 608-Einfüger ─► hlssink3 v1.m3u8
            └► videoscale ─► venc_r2 ─► …                        ─► hlssink3 v2.m3u8
Ton ae<i> ─► hlssink3 a<i>.m3u8 (je Tonspur eine Rendition)
```

- Jede Stufe hat einen eigenen Encoder derselben Bauart wie der Haupt-Encoder (VideoToolbox am Mac, sonst die Rückfallkette bis x264). Scheitert ein Encoder beim Start, versucht der Kanal die ganze Pipeline mit dem nächsten.
- Stufen: frei als `Höhe:kbit/s`; leer gilt der Vorschlag (zwei Stufen unter der Hauptauflösung, Bitrate nach Bildfläche hoch 0,75). Die Breite folgt dem Seitenverhältnis, höchstens vier Stufen.
- Mit eingebrannter Grafik (`burn = all`) kommen alle Stufen aus dem eingebrannten Bild.

## Gleiche Keyframes in allen Stufen

Umschalten zwischen Stufen setzt voraus, dass jedes Segment in jeder Stufe am selben Bild beginnt. Drei Dinge sorgen dafür:

1. **Gemeinsames Raster** (`hls::KeyAlign`): Am Eingang jedes Encoders sieht eine Sonde die PTS. Überschreitet ein Bild eine Rastergrenze (Vielfache der GOP-Länge in Bildern), geht vor diesem Bild ein Force-Key-Unit-Befehl an den Encoder. Alle Encoder hängen am selben Tee, sehen also dieselben PTS und entscheiden gleich. Fehlt das Bild genau auf der Grenze, wird das nächste Keyframe – in allen Stufen.
2. **Keine Einzelanforderungen:** Die Segmentierer fordern mit Leiter keine eigenen Keyframes an (`send-keyframe-requests=false`), x264 setzt keine Szenenwechsel-Keyframes (`scenecut=0`). Ein Keyframe von außen (RTMP-Start, `/api/keyframe`) wird zwei Bilder später in allen Stufen gleichzeitig gesetzt.
3. **Gleiche Segmentgrenzen** folgen daraus: jeder Segmentierer schneidet am ersten Keyframe nach der Zieldauer.

Geprüft in `tests/roundtrip.rs::hls_abr_ladder_aligned_keyframes` mit ffprobe (Keyframe-PTS aller Segmente je Playlist), mit VideoToolbox und mit x264: in beiden Fällen identische Keyframe-PTS in allen drei Stufen.

## Last am M1 Pro

`python3 scripts/abr/lastmessung.py target/release/lz-streamer 30` – Testbild mit Uhr, 50 fps, HLS (TS, 2 s), ein Stereo-Ton, CPU als Anteil eines Kerns für den ganzen Prozess.

MESSUNG

- Die Stufen kosten am Mac vor allem CPU fürs Herunterskalieren (bilinear, mehrere Threads); das Codieren selbst läuft in der Media-Engine.
- Das UHD-Testbild entsteht in HD und wird auf der CPU hochskaliert (Lanczos). Das kostet schon ohne Leiter den Großteil; an einem echten UHD-Eingang fällt dieser Schritt weg.

## Diskontinuität (HLS, RTMP)

- **Standbild bei Signalverlust:** `fallbackswitch` liefert das Standbild im selben Rohformat und auf derselben Zeitachse; Encoder und Segmentierer laufen durch. Gemessen: nach Signalverlust und Rückkehr haben alle Pakete der Bild- und Tonsegmente gleiche Abstände (3600 bzw. 1920 Takte bei 25 fps/48 kHz), keine Lücke, kein Sprung. Eine `#EXT-X-DISCONTINUITY`-Marke ist dafür nicht nötig. RTMP hängt am selben Encoder und läuft ebenso ununterbrochen weiter.
- **Formatwechsel am Eingang:** wird vor dem Umschalter auf das Ausgangsformat skaliert; der Ausgang ändert sein Format nicht.
- **Kanal-Neustart** (Absturz, geänderte Einstellungen): Der Segmentierer beginnt wieder bei Sequenz 0, hls.js blieb daran stehen (geprüft). Darum schreiben die Segmentierer in `X.raw.m3u8`, und `hlsplaylist` veröffentlicht `X.m3u8`: Segmente des vorigen Laufs bleiben stehen, die Sequenz zählt weiter, vor dem ersten neuen Segment steht `#EXT-X-DISCONTINUITY`, `EXT-X-DISCONTINUITY-SEQUENCE` zählt mit. Segmentnamen tragen eine Kennung je Start. Fortgesetzt wird nur, wenn die alte Playlist jünger als die doppelte Playlist-Dauer plus 30 s ist. Beim Anhalten schreibt der Segmentierer kein `ENDLIST`. Geprüft: Chrome spielt über einen Neustart hinweg weiter (`scripts/hls/wiedergabe-pruefen.mjs … --neustart`). RTMP baut nach einem Neustart eine neue Verbindung auf.

## Hochladen (HTTP PUT)

`hlssink3` schreibt nur Dateien. `hlsupload` beobachtet den Ordner und lädt je Playlist zuerst alle genannten Segmente (bei CMAF auch das Init-Segment), danach die Playlist; die Master-Playlist zuletzt. Jede Datei bis zu drei Versuche, Fehlgeschlagenes im nächsten Durchgang erneut. Übertragung mit `curl` (HTTPS, eine frei wählbare Kopfzeile, z. B. für ein Token). Optional löscht er herausgefallene Segmente per HTTP DELETE.

## Wiedergabe geprüft

| Player | Stand |
|---|---|
| hls.js 1.6.16 in Chrome (headless, Remote-Debugging) | spielt ab; drei Stufen, zwei Sprachen, CC1 als Textspur; Wechsel von Stufe und Tonspur wirkt; läuft über Kanal-Neustart weiter |
| Safari (eingebaut, `/hls-test?nativ`) | offen: Safari ließ sich nicht fernsteuern („Entfernte Automation erlauben“ ist aus); von Hand prüfen |

## Offen

- **Safari-Abnahme** von Hand.
- **DASH:** `dashsink` (gst-plugins-bad 1.28) legt jede Spur als eigenes AdaptationSet ohne Sprachkennung an, die Bildstufen wären also nicht als Leiter umschaltbar, und der mp4-Muxer ist als „deprecated, non-functional“ markiert. Sinnvoller ist ein eigenes MPD über die CMAF-Segmente von `hlscmafsink` (gleiche Segmente für HLS und DASH).
