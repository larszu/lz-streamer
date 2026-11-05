# Sprache / Musik / Pause (A3) – Stand und Messung

Stand: 05.11.2026. Code in `src/vad.rs`, Prüfwerkzeug `lz-streamer klassen-messen --korpus <ordner>`. Der Korpus wird mit `scripts/vad/korpus-bauen.py` gebaut und liegt außerhalb des Repos.

## Verfahren

Das Verfahren arbeitet mit klassischen Merkmalen je 1-s-Fenster bei 16 kHz mono, ohne Modelldatei:
- Anteil aktiver Rahmen (> −50 dBFS)
- Pegelschwankung (Standardabweichung über das Fenster)
- spektrale Flachheit (FFT 512)
- Stimmhaftigkeit per Autokorrelation (70–800 Hz)
- Tonhöhen-Stabilität

Die Entscheidung fällt nach festen Regeln:

| Bedingung | Klasse |
|---|---|
| aktiver Anteil < 15 % | Stille |
| Flachheit > 0,25 | Applaus |
| stimmhaft > 60 % und Schwankung < 5,5 dB | Gesang |
| stimmhaft < 30 %, Schwankung < 3 dB, tonal | Orgel |
| sonst | Sprache |

**Rechenzeit:** 86–91 µs je 10 ms Audio am M1 Pro. Das Ziel war < 1 ms.

## Ergebnisse

| Korpus | Stille | Sprache | Gesang | Orgel | Applaus |
|---|---|---|---|---|---|
| Prüfkorpus: Sprache echt (FLEURS DE/EN, ausgesteuert), übrige Klassen **synthetisch** | 100 % | 95,9 % | 100 % | 100 % | 100 % |
| echte Musik (3 lokale Stücke: Band ohne Gesang, Akkordeon, Operngesang) | – | – | 10 % | 0 % | – |

Auf echter Musik werden nur **41,9 %** der Fenster überhaupt als Musik erkannt, also als Gesang oder Orgel.

**Bewertung:**
- Die Regeln trennen den synthetischen Korpus, aber nicht echte Musik.
- Echte Instrumentalmusik ist stimmhaft und schwankt im Pegel. Sie sieht für diese Merkmale aus wie Gesang oder Sprache.
- Die Abnahme aus #5 (≥ 90 % je Klasse) ist damit **nicht erreicht**.
- Stille und Applaus sind auch an echtem Material plausibel. Geprüft ist das nur mit synthetischem Applaus.

## Nächster Schritt

Ein gelerntes Verfahren ist nötig. Kandidaten:
- Silero VAD für Sprache (MIT, ONNX, ≈ 2 MB)
- ein kleines Audio-Klassifikationsnetz für Musik/Applaus, etwa YAMNet (AudioSet-Klassen „Speech“, „Singing“, „Organ“, „Applause“, „Silence“; Apache-2.0)

Beides braucht einen Modell-Download und eine ONNX- oder TFLite-Laufzeit. Das gehört vorher abgestimmt.

Für die Abnahme braucht es außerdem einen echten Korpus mit Gesang, Orgel und Applaus, z. B. MUSAN (CC BY 4.0, ≈ 11 GB) oder eigene Saalaufnahmen.
