# Live-Untertitel: Messung von Wortfehlerrate und Verzug

Stand: 05.11.2026. Gemessen auf einem MacBook Pro M1 Pro mit Whisper `ggml-small.bin` (whisper.cpp über `whispertranscriber`, CPU mit 8 Threads; die GPU-Option bringt mit der Homebrew-Fassung kaum etwas).

Werkzeug: `lz-streamer untertitel-live-messen`, Code in `src/caption_measure.rs`.

## Kurzfassung

Die Werte stammen aus Messreihe 2: jede Äußerung auf −23 LUFS ausgesteuert, Darstellung Roll-up 2 Zeilen, Whisper „small“, Stückgröße 3 s. Format der Verzüge: Median / P95.

| | Deutsch | Englisch |
|---|---|---|
| Wortfehlerrate | **0,15** | **0,10** |
| Verzug Erkennung (Zeile fertig) | **4,5 / 5,6 s** | 4,8 / 6,0 s |
| Verzug am Decoder (608 aus der SEI über SRT) | **7,9 / 9,6 s** | 7,7 / 9,9 s |
| Erstes Wort einer Äußerung → Anzeige am Decoder | 8,0 / 13,8 s | 8,3 / 11,6 s |
| Untertiteldatei neben der Aufnahme | 4,5 / 5,5 s | 5,0 / 6,8 s |
| 608 in der Aufnahme (TS) | 6,4 / 8,3 s | 6,0 / 9,0 s |
| CPU (ganzer Messlauf: Encoder, Erkennung, Decoder) | ≈ 3,3 Kerne | ≈ 3,0 Kerne |

Alle Verzüge gelten gegen den Beginn des ersten Wortes.

**Übersetzung DE → EN** (Whisper-eigen, Messreihe 1 ohne Aussteuerung):
- WER 0,60 gegen die Referenzübersetzung
- Erkennung 4,6 / 6,2 s, Decoder 7,2 / 9,2 s

**Bewertung:**
- Das Ziel „Textverzug Median ≤ 5 s“ aus #11 ist **an der Erkennung erreicht** (4,5 s). Am Decoder-Ausgang ist es nicht erreicht (≈ 7,8 s).
- Die Differenz von ≈ 3 s liegt in der CEA-608-Übertragung und den Warteschlangen, siehe unten.
- Das schlechte Englisch-Ergebnis der ersten Messung (WER 0,72) war ein Pegelproblem des Testmaterials, kein Modellproblem:
  - Einige FLEURS-Sprecher liegen bei −50 … −80 dB. Whisper ließ diese Äußerungen aus.
  - Ausgesteuert liegt Englisch bei WER 0,10.
  - Im Betrieb steuert die Lautheitsregelung (A2, #4) den Ton vor der Erkennung aus. Bis dahin hilft der Pegel der Tonspur.

## Modellvergleich (M1 Pro, CPU)

| Modell | Rechenzeit für 15 s Ton (offline, Stück 3 s / 5 s) | Echtzeit live? | WER DE | WER EN | Verzug Erkennung (Median / P95) | CPU live |
|---|---|---|---|---|---|---|
| **small** | 8,2 s / 6,2 s | **ja** (Stück 3 s) | 0,15 | 0,10 | 4,5 / 5,6 s | ≈ 3 Kerne |
| medium | 34,6 s / 65,2 s | **nein** (2,3–4,3 × Echtzeit) | – | – | – | – |
| large-v3-turbo | 33,4 s / 21,6 s | **knapp nein** (Stück 5 s: 1,4 × Echtzeit) | 0,08 (Lauf 1) / 0,54 (Lauf 2, Rückstau) | 0,13 | 7,9 s (Lauf 1) bis 51 s (Rückstau) | ≈ 6 Kerne |

Lesart:
- Am M1 Pro ist nur „small“ dauerhaft live-fähig.
- „large-v3-turbo“ erkennt Deutsch deutlich besser (WER 0,08 im ersten Lauf). Es rechnet aber langsamer als Echtzeit, und der Verzug wächst mit der Zeit (Lauf 2: bis 77 s).
- Für large-v3-turbo braucht es einen schnelleren Rechner oder eine GPU-Fassung von whisper.cpp (Metal/CUDA). Die Homebrew-Fassung rechnet auf der CPU; `use-gpu=true` änderte die Rechenzeit kaum (9,5 statt 11,2 s).
- „medium“ ist am M1 Pro für live ungeeignet.
- Eine GPU-Last ist nicht gemessen, weil die Erkennung praktisch nur auf der CPU läuft. Die Werte „CPU live“ sind (user + sys) / Laufzeit des ganzen Messlaufs mit Encoder und Decoder.

**Messreihe 1** (ohne Aussteuerung, Pop-on): Deutsch WER 0,21, Erkennung 4,1 / 5,1 s, Decoder 7,0 / 8,5 s. Englisch WER 0,72, weil Äußerungen ausgelassen wurden.

## Aufbau (live, nicht im Stapel)

- **Testvideo:** Bild ist ein laufendes Testbild (testsrc2), Ton sind 12 Äußerungen aus Google FLEURS (dev-Teil, Lizenz CC BY 4.0), dazwischen je 2 s Pause. Deutsch dauert 154 s, Englisch 143 s.
  - Erzeugt mit `scripts/untertitel/testdaten-laden.sh`.
  - Die Daten liegen in `~/Library/Caches/lz-streamer-testdaten/`, nicht im Repo.
  - Der Sprachbeginn jeder Äußerung kommt aus der Stille-Erkennung (−40 dB, 0,15 s).
  - Die Referenz für die Übersetzung sind die englischen FLEURS-Sätze mit derselben ID.
- **Eingang:** Das Video läuft über den neuen Eingang „Videodatei (Echtzeit)“ (`clocksync`) wie ein Live-Eingang durch den Kanal. Die Erkennung läuft mit, genau wie später am SDI-Eingang.
- **Kette:** Tonspur 1/2 → Abgriff 16 kHz mono → Whisper in eigener Teil-Pipeline → Zeilen (≤ 32 Zeichen, Satzende, Pause) → `tttocea608` → `cccombiner` → H.264-SEI. Von dort aus gleichzeitig:
  1. SRT zum Decoder. Der Decoder liest mit `h264ccextractor` → `cea608tojson` (zur Anzeigezeit).
  2. Aufnahme als TS mit 608 in der SEI und dazu eine `.srt`-Untertiteldatei.
  3. Eingebrannt im Bild: Rückfall-Texteinblendung, solange kein OGraf-Renderer verbunden ist (#12).
- WebVTT/HLS gibt es noch nicht (#23). Dieser Weg ist nicht gemessen.

### Was „Verzug“ hier heißt

Gemessen wird gegen den Beginn des ersten Wortes einer Zeile:
- Für Zeilen nimmt das Werkzeug den Wortzeitstempel, den Whisper zu diesem Wort liefert. Er liegt auf derselben Pipeline-Uhr wie Encoder und Decoder.
- Für den objektiven Sprachbeginn je Äußerung nimmt es die Stille-Erkennung auf der Tonspur (Zeile „Erstes Wort einer Äußerung“).

Der Anzeigezeitpunkt am Decoder ist der Moment, in dem `cea608tojson` die Zeile ausgibt. Der Monitor ist nicht enthalten.

## Was die Messung gezeigt hat und behoben ist

1. **Rückstau bei 2 s Stückgröße.**
   - Mit 2 s wuchs der Verzug stetig: am Decoder Median 30 s, P95 44 s.
   - Grund: Whisper rechnet je Aufruf ein ganzes Fenster. Bei 2-s-Stücken (+ 0,5 s Überlappung) schafft „small“ am M1 Pro neben dem Encoder kein Echtzeit.
   - Die Vorgabe ist jetzt 3 s.
   - Offline gemessen (15 s Ton): bei 2 s 9,5 s Rechenzeit, bei 3 s 8,9 s, bei 5 s 5,4 s.
2. **`whispertranscriber` lehnt live eine `latency` über `chunk-duration` ab** und bricht mit Datenstromfehler ab. Jetzt gilt `latency = chunk-duration`.
   - Nebenbefund: Die Erkennung startete wie vorgesehen alle 10 s neu, ohne dass der Kanal abriss.
3. **`n-threads` ist im Element per Vorgabe 1.** lz-streamer setzt 8.
4. **Geräuschbeschreibungen** wie „[Musik]“, „[BLANK_AUDIO]“ oder „(Applaus)“ landeten in den Untertiteln. Jetzt laufen `suppress-nst`/`suppress-blank` und ein Klammerfilter in der Zeilenbildung.
   - Ergebnis: WER Deutsch 0,48 → 0,21.
5. **Decoder-Anzeige:** `cea608tott` gibt eine Pop-on-Zeile erst aus, wenn die nächste beginnt. Jetzt liest der Decoder über `cea608tojson unbuffered=true`, also zur Anzeigezeit.
   - Falle: Das Element verlangt Pufferdauern, die der Extraktor nicht setzt. Eine Sonde setzt 20 ms.
6. **Aufnahme sauber schließen:** Ein EOS an die ganze Pipeline blockierte an der Datei-Quelle. Jetzt geht EOS nur in die Aufnahme-Pads.

## Lässt sich der Abstand Erkennung → Decoder (≈ 3 s) verringern?

Gemessen mit Deutsch, Whisper „small“, Stückgröße 3 s:

| Variante | Erkennung Median | 608 in der Aufnahme | Decoder Median / P95 |
|---|---|---|---|
| Pop-on, ganze Zeilen | 4,2 s | 6,8 s | 7,3 / 8,7 s |
| Roll-up, ganze Zeilen | 4,1 s | 5,9 s | **7,0 / 8,5 s** |
| Roll-up, Zeilen nach Sprechzeit getaktet | 4,2 s | 5,9 s | 7,6 / 9,4 s |
| Roll-up, Wort für Wort | 8,0 s | 8,3 s | 9,5 / 32 s (Rückstau) |

Lesart:

- **Woher die ≈ 3 s kommen:**
  - **Encoderseitig ≈ 1,7 s** (Erkennung → 608 im Strom): CEA-608 überträgt höchstens 2 Byte je Bild (≈ 60 Byte/s). Whisper liefert je Stück mehrere Zeilen auf einmal, die sich dann in `tttocea608` anstellen. Eine Roll-up-Zeile mit 32 Zeichen braucht allein ≈ 0,7 s.
  - **Decoderseitig ≈ 1,1 s:** `cea608tojson` gibt eine Roll-up-Zeile erst mit dem Zeilenende aus. Dazu kommt der Videoweg (≈ 0,25 s).
- **Taktung** nach Sprechzeit (`paced`) verteilt die Stöße, verschiebt dafür aber jede Zeile auf „gesprochen + P90-Verzug“. Unterm Strich bringt sie nichts. Sie bleibt als Option, Vorgabe ist aus.
- **Wort für Wort** erzeugt viele kleine 608-Befehle und belastet dabei die CPU so, dass Whisper nicht mehr mitkommt (Rückstau).
- **Ergebnis:** Roll-up mit ganzen Zeilen ist die beste Einstellung und für die Spracherkennung empfohlen (Darstellung „Roll-up, 2 Zeilen“). Weiter senken lässt sich der Abstand nur über **CEA-708**: Das sind 9600 bit/s statt 60 Byte/s, es geht über `tttocea708` mit `cea608-channel=1`, YouTube und neuere Decoder lesen 708. Ein 708-Leser fehlt in GStreamer (`cea708tojson` gibt es nicht), darum ist der Weg hier noch nicht gemessen.

## Offen

- **Verzug am Decoder ≈ 3 s über der Erkennung.**
  - Bestandteile: 608 überträgt nur 2 Zeichen je Bild (32 Zeichen ≈ 0,6 s bei 25 fps), eine Zeile kommt erst nach ihrem Zeilenende, dazu kommen Warteschlangen in `tttocea608` und im Decoder-Zweig.
  - Abhilfe: Wörter im Roll-up einzeln senden statt ganzer Zeilen (spart bis zu einer Zeilenlänge); die Decoder-Warteschlange kürzen; 708-Dienst (höhere Datenrate) zusätzlich.
- **Leise Eingänge:** Ohne Aussteuerung lässt Whisper leise Sprache aus (siehe Englisch, Messreihe 1). Vor der Erkennung gehört eine Pegelanpassung (#4).
- **Übersetzung DE→EN** (Whisper-eigen): WER 0,60 gegen die Referenzübersetzung. Für Übersetzungen ist die WER ein grobes Maß. Der Inhalt ist verständlich, der Verzug gleicht der Erkennung.
- large-v3-turbo live nur mit schnellerer Hardware oder GPU-Fassung von whisper.cpp.
- YouTube-Test mit „Embedded 608/708“ braucht einen Live-Kanal.

## Nachmessen

```bash
scripts/untertitel/testdaten-laden.sh 12          # einmalig, ≈ 400 MB FLEURS dev (CC BY 4.0)
D=~/Library/Caches/lz-streamer-testdaten
M="$HOME/Library/Application Support/lz-streamer/modelle/ggml-small.bin"
./target/release/lz-streamer untertitel-live-messen --video $D/de_de.mp4 --onsets $D/de_de.onsets.tsv \
  --referenz $D/de_de.ref.txt --modell "$M" --sprache de --modus roll-up2 --chunk 3000
# Übersetzung: --referenz $D/de_de.ref.en.txt --uebersetzen
```

## Untertiteldateien zur Aufnahme (U5)

Stand: 05.11.2026. Neben jedem Aufnahme-Segment entstehen `.srt` und `.vtt`, die beim Mitschnitt fortlaufend neu geschrieben werden.
- **Beginn einer Zeile:** Die Zeit stammt nicht aus der Einspeisung, sondern aus dem fertigen 608-Strom. `tttocea608` → `cea608tojson` meldet, wann die Zeile sichtbar wird.
- **Ende einer Zeile:** die eingespeiste Anzeigedauer, spätestens die nächste Zeile.
- **Bezugspunkt:** der Segmentbeginn (`running-time` aus `splitmuxsink-fragment-opened`).

Abgleich mit den 608-Untertiteln, die ffmpeg (`movie=…[out0+subcc]`) aus der Aufnahme liest, Pop-on, 25p:

| Zeile | Abweichung ffmpeg − Datei |
|---|---|
| erste | −68 … −9 ms |
| ab der zweiten | +148 … +229 ms |

**Bewertung:**
- Ab der zweiten Zeile setzt ffmpeg Beginn *und* Ende gleichmäßig rund 150–230 ms später als `cea608tojson`.
- Ein unabhängiger Player (VLC) fehlt hier, deshalb ist offen, welche Seite die Anzeige genauer trifft.
- Die Abnahme „< 200 ms“ ist damit gegenüber ffmpeg nicht sicher erreicht.

**Nicht umgesetzt:**
- `.scc`
- **SDI-Untertitel:** Sie kommen ohne Text-Zweig und landen nicht in den Dateien.
- **WebVTT-Spur in HLS:** Sie kommt mit der HLS-Ausgabe (#23).
