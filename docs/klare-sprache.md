# Klare Sprache (K1)

Stand: 05.11.2026. Code in `src/clearspeech.rs`, Element `lzclearspeech`.

## Verfahren

DeepFilterNet3 trennt je 10-ms-Block die Sprache `s` vom Rest `r = x − s`. Rest heißt hier Musik, Geräusch und Hall. Ausgegeben wird `s + g·r`, also `(1 − g)·s + g·x`:
- `x` ist das Original, um die Modell-Latenz verzögert.
- `g` ist der eingestellte Rest-Pegel, Vorgabe −9 dB.
- Danach folgt die Lautheitsregelung der Spur.

Ist „Bei Gesang nicht absenken“ an und erkennt die Klassifizierung aus A3 Gesang, läuft `g` mit 0,5 s Zeitkonstante auf 0 dB.

Das Modell steckt im Crate `deep-filter-rt` 0.1.1 (MIT/Apache-2.0). Es ist eine Kopie von libDF aus DeepFilterNet mit eingebettetem DeepFilterNet3-Modell. Zur Laufzeit wird nichts geladen.

Die Spur ist eine eigene Tonspur mit eigenem Namen und eigener Sprachkennung. „Klare-Sprache-Spur hinzufügen“ übernimmt Quelle und Sprache der ersten Spur. Die Hauptspur ist ein eigener Zweig hinter dem Eingangs-Tee und bleibt unverändert; ein Test prüft, dass ihr Pipeline-Abschnitt gleich bleibt.

## Gemessen (MacBook Pro M1 Pro)

| Punkt | Ziel | Ergebnis |
|---|---|---|
| Zusatzlatenz | ≤ 30 ms | 30,0 ms (Latenzabfrage des Elements) |
| Lippensynchron | Versatz zur Hauptspur ≈ 0 | 0,00 ms (Klick-Test, Zeitstempel um die Modell-Latenz zurückgesetzt) |
| Rechenlast | < 1 Kern zusätzlich | 3,7 % eines Kerns (Stereo, Release-Bau) |
| 1080p50-Kanal + Spur | Echtzeit | 50,4 Bilder/s, Rest −9 dB |
| reines Rauschen | Rest um ≈ 9 dB leiser | −6 … −12 dB (Unit-Test) |

## Offen

- **AV-Messung mit lz-scopes** an echter Strecke steht aus. Gemessen ist nur der Versatz der Zeitstempel in der Pipeline.
- **Hörtest mit 3 Saalaufnahmen:** Die Aufnahmen fehlen, das Protokoll macht Lars.
- **Gesangserkennung:** Sie hängt an A3. A3 erkennt echte Musik schlecht (siehe [sprache-musik-pause.md](sprache-musik-pause.md)) und hält den 1-kHz-Testton für Gesang. Bis ein gelerntes Verfahren da ist, schaltet „Bei Gesang nicht absenken“ unzuverlässig.
- **Kennzeichnung der Spur:** Die Spur trägt dieselbe Sprachkennung wie die Quelle. Den TS-Audiotyp „hearing impaired“ (ISO-639-Deskriptor) setzt `mpegtsmux` nicht.
