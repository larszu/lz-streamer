# Hochskalieren HD → UHD: Zeit, Last und Qualität

Stand: 05.11.2026. Gemessen auf einem MacBook Pro M1 Pro (8 Leistungs- und 2 Effizienzkerne), GStreamer 1.28.8, ffmpeg mit libvmaf. Messskript: `scripts/skalierung/messen.sh`. Hintergrund zu Hardware-Skalierern: `recherche-hardware-scaler.md`.

## Kurzfassung

- **Zeit:** Der Skalierer der Pipeline (`videoscale`, 8 Threads) braucht für ein Bild 1080p → 2160p:
  - Lanczos e2: **3,2 ms**
  - Lanczos e3: **4,4 ms** (neue Vorgabe)
  - bilinear: **2,4 ms**

  Er puffert keine Bilder. Sein Latenzbeitrag ist also genau diese Rechenzeit, weit unter einem Bildtakt (20 ms bei 50p). In der Gesamtmessung von Testbild bis Decoder geht er im Rauschen der Bildtakte unter.
- **Last:** Lanczos e3 kostet 34 ms CPU-Zeit je Bild. Bei 2160p50 sind das ≈ 1,7 von 10 Kernen; einthreadig wären es nur ≈ 23 fps.
- **Qualität:** Die drei klassischen Filter liegen eng beieinander: Lanczos aus GStreamer, Lanczos aus ffmpeg und Bicubic.
  - Lanczos mit Hüllkurve 3 statt 2 bringt +0,3 dB PSNR-Y und +0,8 VMAF-NEG. Das ist ab sofort die Vorgabe für „Hochwertig“.
  - Echte Fortschritte erreicht keiner davon. Die von der Recherche vorgeschlagenen Zielwerte (PSNR-Y +1,5 dB, VMAF +3…+6) verfehlen alle klassischen Filter. Das bestärkt den Weg zu einem kantenadaptiven Skalierer auf der GPU (§5).
- **Nachschärfen täuscht VMAF:** `sharpen=0.3` hebt das 4K-VMAF auf 100, zerstört aber PSNR (−15 dB) und VMAF-NEG (−24). Bewertet wird deshalb mit VMAF-NEG, das Verbesserungs-Effekte nicht belohnt, zusammen mit PSNR und SSIM.

## Verfahren

1. **Referenz:** 50 Bilder UHD in begrenztem Wertebereich, 8 bit 4:2:0. Die erste Messung mit Vollbereich-Referenz ergab falsche 36 dB, weil GStreamer in den begrenzten Bereich umrechnet.
2. **HD-Fassung:** einheitlich mit ffmpeg-Lanczos heruntergerechnet.
3. **Hochskalieren** mit jedem Kandidaten, danach Vergleich gegen die Referenz. VMAF läuft mit dem 4K-Modell `vmaf_4k_v0.6.1` und dem NEG-Modell `vmaf_v0.6.1neg`, dazu PSNR-Y und SSIM. NEG gibt es nur als HD-Modell; es dient hier dem relativen Vergleich.
4. **Zeit:** Lauf mit Skalierer minus Lauf ohne, geteilt durch 50 Bilder. CPU aus `time` (user).

Zwei Inhalte:
- **Kamera:** eigene Panasonic-Aufnahme, UHD50 10 bit V-Log, Innenraum mit Regal und Text; flach und detailreich.
- **Testbild:** synthetischer UHD25-Clip mit Farbbalken, Diagonale und Schachbrett aus `lz-multicam-sync/out/media_test`; harte Kanten.

Beide Dateien bleiben lokal und liegen nicht im Repo.

## Ergebnisse

### Qualität (höher = besser)

| Skalierer | Kamera VMAF-4K | Kamera VMAF-NEG | Kamera PSNR-Y | Testbild VMAF-4K | Testbild VMAF-NEG | Testbild PSNR-Y |
|---|---|---|---|---|---|---|
| GStreamer bilinear („Schnell“) | 91,76 | 85,88 | 48,85 | 80,67 | 72,62 | 37,57 |
| GStreamer Lanczos e2 (bisher „Hochwertig“) | 97,69 | 93,52 | 51,21 | 89,09 | 82,47 | 38,16 |
| **GStreamer Lanczos e3 (neu „Hochwertig“)** | 98,35 | 94,30 | 51,53 | 90,09 | 83,28 | 38,12 |
| GStreamer Lanczos e4 | 98,20 | 94,19 | 51,60 | 89,99 | 83,21 | 38,09 |
| GStreamer Lanczos e3, sharpness 1.2 | 99,99 | 94,80 | 50,81 | 93,68 | **87,06** | **38,87** |
| GStreamer Catmull-Rom | 98,84 | 94,33 | 51,29 | 90,42 | 83,45 | 38,33 |
| GStreamer 4-tap | 100,00 | 90,19 | 48,27 | 99,09 | 80,88 | 37,60 |
| GStreamer Lanczos, sharpen 0.3 | 100,00 | 69,54 | 36,41 | 100,00 | 69,05 | 31,63 |
| ffmpeg bicubic | 98,94 | 94,44 | 51,64 | 90,28 | 83,19 | 38,21 |
| ffmpeg Lanczos | 99,59 | **94,96** | 51,89 | 91,04 | 83,83 | 38,21 |
| ffmpeg spline | 99,14 | 94,86 | **51,92** | 90,53 | 83,55 | 38,18 |

SSIM liegt bei allen außer den nachgeschärften Varianten zwischen 0,9995 und 1,0000 und trennt nicht.

Lesart:
- Gegen das bisherige Lanczos e2 bringt keiner der klassischen Filter mehr als +0,7 dB PSNR-Y oder +4,6 VMAF-NEG. Den höchsten Wert erreicht e3 mit `sharpness 1.2` am Testbild. An der flachen Kamera-Aufnahme verliert diese Variante aber 0,4 dB PSNR. Als Vorgabe ist sie deshalb nicht übernommen.
- ffmpeg-Lanczos ist leicht besser als GStreamer-Lanczos e2: rund +0,7 dB und +1,4 VMAF-NEG an der Kamera. Mit e3 schrumpft der Abstand auf +0,4 dB und +0,7.
- Das 4K-VMAF sättigt bei der Kamera-Aufnahme (V-Log, wenig Kontrast) nahe 100 und belohnt Nachschärfen. Für Entscheidungen zählen deshalb VMAF-NEG und PSNR.

### Zeit und Last (1080p → 2160p, NV12, 8 Threads)

| Skalierer | Zeit je Bild (= Latenzbeitrag) | CPU je Bild | Kerne bei 2160p50 |
|---|---|---|---|
| bilinear | 2,4 ms | 16,8 ms | 0,8 |
| Lanczos e2 | 3,2 ms | 23,8 ms | 1,2 |
| Lanczos e3 | 4,4 ms | 33,8 ms | 1,7 |
| Lanczos e4 | 6,6 ms | 42,4 ms | 2,1 |
| Catmull-Rom | 3,0 ms | 24,0 ms | 1,2 |
| Lanczos e2, 1 Thread | 21 ms | 22 ms | – (nur ≈ 23 fps) |

**In der laufenden Pipeline** gemessen mit `lz-streamer latenz --h265 --format …`: Die Station „Quelle“ (Testbild, Skalierer, Umschalter) liegt bei 1080p50 bei 33 ms und bei 2160p50 mit Hochskalieren bei 25 ms. Der Skalierer verschwindet also im Rauschen des Bildtakts.

Nebenbefund: Mit `vtdec_hw` dauert die Station SRT+Demux+Decode bei H.265 450–460 ms bei 50p und 826 ms bei 25p. Der Hardware-Decoder hält rund 20 Bilder zurück. Das ist der größte Latenzposten bei H.265 am Mac und steht als offener Punkt in der README.

## Nicht gemessen

- **RAVU/FSRCNNX und libplacebo:** Die Homebrew-Fassung von ffmpeg ist ohne libplacebo gebaut, mpv ist nicht installiert. Ein solcher Pfad bräuchte mpv und libplacebo samt Shader-Dateien aus fremden Quellen. Das ist „großer Aufwand“ im Sinne des Auftrags und bleibt offen.
- **zscale:** Die ffmpeg-Fassung ist ohne zimg gebaut. Spline liegt als `flags=spline` vor und ist oben gemessen.
- **10 bit durchgehend:** Die Pipeline arbeitet heute in NV12, also 8 bit. Gemessen wurde passend dazu in 8 bit.

## Folgerung

1. **Sofort:** „Hochwertig“ nutzt jetzt `method=lanczos envelope=3`. Das kostet +1,2 ms je Bild und bringt messbar mehr ohne Nebenwirkung.
2. **Einen neuen Skalierer bauen lohnt sich nur als GPU-Verfahren mit kantenadaptivem 2× wie RAVU oder FSRCNNX, mit Antiringing und in 10 bit.** Mit klassischen Filtern ist das Ziel von +1,5 dB nicht zu erreichen, die Zahlen hier zeigen höchstens +0,7 dB. Vor dem Umbau wäre ein libplacebo-Prüfstand nötig, über ffmpeg mit libplacebo oder mpv, um den Gewinn erst zu messen.
3. Den Einfluss von Nachschärfen immer mit VMAF-NEG und PSNR prüfen, nie mit dem 4K-VMAF allein.
