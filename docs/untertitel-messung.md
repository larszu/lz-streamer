# Live-Untertitel: Messung von Wortfehlerrate und Verzug

Stand: 05.11.2026. Gemessen auf einem MacBook Pro M1 Pro mit Whisper `ggml-small.bin` (whisper.cpp über `whispertranscriber`, CPU mit 8 Threads; die GPU-Option bringt mit der Homebrew-Fassung kaum etwas).

Werkzeug: `lz-streamer untertitel-live-messen`, Code in `src/caption_measure.rs`.

## Kurzfassung

| | Deutsch | Englisch | Deutsch → Englisch (Übersetzung) |
|---|---|---|---|
| Wortfehlerrate der Erkennung | **0,21** | 0,72 (siehe unten) | 0,60 gegen Referenzübersetzung |
| Verzug Erkennung (Zeile fertig) | Median **4,1 s**, P95 5,1 s | 4,6 s / 6,5 s | 4,6 s / 6,2 s |
| Verzug am Decoder (608 aus der SEI) | Median **7,0 s**, P95 8,5 s | 8,3 s / 10,9 s | 7,2 s / 9,2 s |
| Erstes Wort einer Äußerung → Anzeige am Decoder | Median 7,5 s, P95 17,8 s | 8,4 s / 16,2 s | 6,9 s / 10,1 s |
| Untertiteldatei neben der Aufnahme | Median 4,2 s, P95 5,3 s | 4,4 s / 6,1 s | 4,4 s / 6,4 s |
| 608 in der Aufnahme (TS) | Median 5,9 s, P95 8,0 s | 7,7 s / 12,6 s | 6,1 s / 11,0 s |

Alle Zeilen außer der Wortfehlerrate sind Verzüge gegen den Beginn des ersten Wortes, Werte im Format Median / P95. Darstellung Roll-up, 2 Zeilen, Stückgröße 3 s.

Mit Pop-on statt Roll-up liegt der Decoder-Verzug bei Deutsch bei 7,3 s (P95 8,7 s).

**Bewertung:**
- Das Ziel aus #11, „Textverzug Median ≤ 5 s“, ist an der Erkennung erreicht (4,1 s). Am Decoder-Ausgang ist es nicht erreicht (7,0 s).
- Die Differenz von ≈ 3 s entsteht auf dem Weg Erkennung → CEA-608 → SEI → Decoder. Ursachen und Abhilfen stehen unten.
- Deutsch erkennt Whisper „small“ brauchbar (WER 0,21). Fehler sind vor allem Komposita und Namen, etwa „WLAN-Türblinge“ statt „WLAN-Türklingel“. Dafür gibt es die Ersetzungsliste.

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

## Offen

- **Verzug am Decoder ≈ 3 s über der Erkennung.**
  - Bestandteile: 608 überträgt nur 2 Zeichen je Bild (32 Zeichen ≈ 0,6 s bei 25 fps), eine Zeile kommt erst nach ihrem Zeilenende, dazu kommen Warteschlangen in `tttocea608` und im Decoder-Zweig.
  - Abhilfe: Wörter im Roll-up einzeln senden statt ganzer Zeilen (spart bis zu einer Zeilenlänge); die Decoder-Warteschlange kürzen; 708-Dienst (höhere Datenrate) zusätzlich.
- **Englisch WER 0,72:**
  - Whisper ließ ganze Äußerungen aus: 107 statt 271 Wörtern. In Pausen erfand es dafür „Thank you.“.
  - Die erkannten Sätze selbst stimmen fast wörtlich.
  - Nächster Schritt: eine VAD vor der Erkennung (Silero) und Prüfung, ob `live-edge-offset` Stücke verwirft.
- **Übersetzung DE→EN** (Whisper-eigen): WER 0,60 gegen die Referenzübersetzung. Für Übersetzungen ist die WER ein grobes Maß. Der Inhalt ist verständlich, der Verzug gleicht der Erkennung.
- Ein größeres Modell (large-v3-turbo) ist nicht gemessen. Bei 5 s Stückgröße wäre es am M1 Pro ein Kandidat.
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
