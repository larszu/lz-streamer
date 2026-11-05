# Recherche: Audioverarbeitung, Klare Sprache und Live-Untertitel für lz-streamer

Stand 05.11.2026. Ziel ist, die Funktionen teurer Broadcast-Audio- und Untertitelgeräte (Jünger D\*AP/AIXpressor, Dolby, AI-Media, ENCO) in lz-streamer zu bekommen. Grundlage sind Hersteller- und Projektseiten, Fachpresse, Handbücher und eine Prüfung der lokalen GStreamer-Installation.

**Kennzeichnung**
- **[H]** Herstellerangabe, nicht unabhängig geprüft.
- **[L]** lokal geprüft: GStreamer 1.28.8 (Homebrew) auf dem MacBook Pro M1 Pro, per `gst-inspect-1.0` oder im Quelltext.
- **[V]** Vermutung oder eigene Abschätzung, nicht belegt.
- **[D]** Dritte, also Händler, Fachpresse oder Aggregatoren.

---

## 0. Kurzfassung

- **Untertitel sind fast fertig im Baukasten.** GStreamer 1.28 bringt alles mit, was lz-streamer braucht [L]:
  - Spracherkennung lokal (`whispertranscriber`)
  - Anbindung von Cloud-Diensten (`speechmaticstranscriber`, `deepgramtranscriber`, `awstranscriber`)
  - `transcriberbin` und `translationbin`
  - Text nach CEA-608/708 (`tttocea608`, `tttocea708`), Einfügen in H.264/H.265-SEI (`h264ccinserter`, `h265ccinserter`)
  - CC aus dem DeckLink-VANC (`decklinkvideosrc output-cc`) und zurück ins SDI (`decklinkvideosink cc-line`)
  - Untertitel einbrennen (`cea608overlay`, `cea708overlay`), DVB-Untertitel (`dvbsubenc`), WebVTT (`webvttenc`, `jsontovtt`)
  - Neu zu bauen sind nur Bedienung, Weiterleitung und Tests.
- **Klare Sprache live in unter 50 ms** geht offen nur mit Modellen, die für Störgeräusche gebaut sind. DeepFilterNet (MIT/Apache, 20 ms ohne Lookahead) und RNNoise (BSD) gehören dazu. Gute Trennung von Sprache und Musik wie bei Dialog+ (Fraunhofer) oder HTDemucs braucht Sekunden.
  - **Vorschlag:** Klare Sprache als **zweite Tonspur** im TS, wie bei der ARD.
  - Modus „latenzarm“ mit DeepFilterNet und Rückmischung des Rests
  - Modus „Qualität“ mit wenigen Sekunden Verzögerung für Streams, deren Latenz egal ist
- **Level Magic (Jünger)** ist laut Handbuch kein Geheimnis. Es kombiniert:
  - eine langsame adaptive AGC (≤ 1 dB je 5 s)
  - einen schnellen Transientenprozessor, gesteuert durch den Vergleich von QP- und RMS-Detektor
  - einen Brickwall-Limiter mit etwa 2 ms Lookahead

  **Ein Patent dazu fand sich nicht** (§3.3). Mit einem eigenen Rust-Element ist das nachbaubar, bei unter 5 ms Latenz. Das fertige `audioloudnorm` in GStreamer taugt live nicht: Es hat 3 s Lookahead [L].
- **Plugins:**
  - VST3 steht seit Oktober 2025 unter MIT [H]. Damit ist ein eigener VST3-Host lizenzrechtlich einfach.
  - CLAP über `clack-host` (MIT/Apache).
  - LV2 und LADSPA fehlen in der Homebrew-GStreamer-Installation [L].
  - Plugins laufen am besten in einem eigenen Hilfsprozess, weil ein abstürzendes Plugin sonst den Kanal mitreißt. Das passt zum Aufpasser-Muster von lz-streamer.
- **Wiederverwenden:**
  - aus lz-vision-mixer der Kanalzug `lzstrip` (Rust-GStreamer-Element mit RNNoise, Gate und Kompressor), den OGraf-Renderer samt `lz-captions`-Grafik und die GStreamer-Fallen
  - aus lz-scopes Lautheit, True Peak, LRA und VANC als geprüfte Referenz mit Tests
  - aus lz-presenter und Broadcast-intercom den OGraf-Weg und die Untertitelquelle per OSC
- **Marktlücke:** Ein Gerät für SRT-Encoder, lokale deutsche Live-Untertitel, Lautheitsregelung und Klare-Sprache-Spur, ohne Cloud-Abo. Die Wettbewerber trennen das:
  - Encoder reichen Untertitel nur durch.
  - Untertitel-Encoder sind Abo-Hardware mit Cloud-Spracherkennung.
  - Audioprozessoren können weder Encoder noch Untertitel.

---

## 1. Klare Sprache und Dialogverbesserung

### 1.1 Das Vorbild: MPEG-H Dialog+ (Fraunhofer IIS) und „Klare Sprache“ der ARD

- Dialog+ trennt per Deep Learning den Dialog aus einer fertigen Mischung und mischt ihn mit abgesenktem Hintergrund neu. Damit lässt sich der Sprachpegel auch bei Material personalisieren, das nur als fertige Mischung vorliegt [H] ([arXiv 2112.09494](https://arxiv.org/abs/2112.09494)).
- **Modell:** ein tiefes Faltungsnetz (Deep Convolutional Neural Network), trainiert auf Sendematerial von WDR und anderen ARD-Anstalten [H] ([Fraunhofer Digital Media](https://www.digitalmedia.fraunhofer.de/en/mediainformation/trendbrochures/trendbrochure-2022/mpeg-h-dialog-plus-technology.html)).
- **Ausgabe:** eine Stereomischung für Altwege und eine ADM-Datei für Next Generation Audio [H] (ebd.).
- **Feldtest:**
  - Ab September 2020 mit WDR und BR, danach „Klare Sprache“ in der ARD-Mediathek.
  - In der Umfrage (> 2000 Teilnehmende) gaben 90 % der über 60-Jährigen an, Sprache im Fernsehen oft schlecht zu verstehen.
  - 83 % mochten die Umschaltmöglichkeit.
  - Quelle: [arXiv 2112.09494](https://arxiv.org/abs/2112.09494).
- ARTE ergänzte eine Gesangserkennung, damit gesungene Passagen nicht wie Dialog behandelt werden [H] ([Fraunhofer IIS 2025](https://www.iis.fraunhofer.de/en/pr/2025/pressrelease-mpeg-h-arte.html)).
- **Latenz:** Die Quellen beschreiben Dialog+ als dateibasiertes Verfahren für fertige Mischungen. Eine Echtzeitfähigkeit ist nicht angegeben [V].

**Folgerung:** Das Prinzip ist Trennung → Hintergrund um X dB absenken → neu mischen → als eigene Spur ausliefern. Eine eigene Tonspur mit Sprachkennung beherrscht lz-streamer schon (`src/audio.rs`, bis 8 Spuren, ISO 639). Klare Sprache ist damit eine weitere Spur mit eigener Verarbeitung vor dem AAC-Encoder.

### 1.2 Offene Modelle im Vergleich

| Modell | Zweck | Latenz | Lizenz | Live < 50 ms? | Quelle |
|---|---|---|---|---|---|
| **DeepFilterNet 3** | Sprache aus Störgeräusch, 48 kHz, Rust-Kern (`libDF`), LADSPA-Plugin | 20 ms (Modell ohne Lookahead, STFT); Paper-Modell mit 2 Frames Lookahead: 40 ms | MIT oder Apache-2.0 (Code); Lizenz der Gewichte nicht gesondert geprüft | **ja** | [GitHub Rikorose/DeepFilterNet](https://github.com/Rikorose/DeepFilterNet), [arXiv 2305.08227](https://arxiv.org/pdf/2305.08227.pdf) |
| **RNNoise** | Rauschunterdrückung, 48 kHz mono | Frame 10 ms (laut Paper von Valin) [V: Zahl aus [arXiv 1709.08243](https://arxiv.org/abs/1709.08243), nicht nachgelesen] | BSD-3-Clause | ja, aber Musik gilt nicht als „Rauschen“ → für Dialog vor Musikbett schwach [V] | [GitHub xiph/rnnoise](https://github.com/xiph/rnnoise); in GStreamer als `audiornnoise` [L] |
| **Demucs / HTDemucs** | Musik-Quellentrennung (4 Stems) | Segmente bis 7,8 s (Hybrid Transformer); CPU ≈ 1,5 × Dauer | MIT; Repo **nicht mehr gepflegt** (Fork adefossez/demucs) | **nein**; mit einigen Sekunden Puffer auf GPU denkbar [V] | [GitHub facebookresearch/demucs](https://github.com/facebookresearch/demucs) |
| **Causal Demucs (denoiser)** | Sprachverbesserung, echtzeitfähig auf Laptop-CPU | jeder zusätzliche Puffer-Frame +16 ms | – (nicht geprüft) | grenzwertig | [PyPI denoiser](https://pypi.org/project/denoiser/) |
| **HS-TasNet** | Musiktrennung, Hybrid Spektrogramm/Wellenform | 23 ms, SDR 4,65 dB (MusDB) | Forschung, kein fertiger Code geprüft | ja, Qualität deutlich unter Offline-Modellen | [arXiv 2402.17701](https://arxiv.org/abs/2402.17701) |
| **Mel-RoFormer, Open-Unmix, Conv-TasNet** | Offline-Trennung bzw. Sprachtrennung | keine Echtzeitangaben gefunden | – | **nicht belegt**; Mel-RoFormer arbeitet nach Kenntnis segmentweise [V] | – |
| **NVIDIA Maxine AFX** | Rauschen/Hall entfernen, Studio Voice | Verzögerung 1–2 Frames, Beispiel 5 ms; Studio Voice 48 kHz: 80 ms algorithmisch [H] | proprietär, RTX-GPU | Denoise ja, Studio Voice nein; nur NVIDIA | [NVIDIA MAXINE-AFX-SDK](https://github.com/NVIDIA/MAXINE-AFX-SDK), [Release R14](https://forums.developer.nvidia.com/t/maxine-r14-release-notes-and-highlights/318615) |
| **AudioShake SDK** (kommerziell) | Echtzeittrennung | 11 ms [H] | proprietär | laut Hersteller ja | [audioshake.ai/products/sdk](https://www.audioshake.ai/products/sdk) |

**Einschätzung [V]:**
1. **Latenzarm (≤ 30 ms):**
   - DeepFilterNet liefert eine Sprachschätzung `s`. Der Rest `r = x − s` (zeitlich ausgerichtet) ist der Hintergrund.
   - Klare-Sprache-Spur: `s + g·r` mit g ≈ −6 … −12 dB, dazu Lautheitsregelung auf Sprache (§3.5).
   - Das ist kein Dialog+, wirkt aber bei Saalton, Lüftung, Applaus und Publikum. Diese Störungen sind bei Lars' Zielgruppe (Kirche, Messe, Firmenevent) häufiger als Filmmusik.
   - DeepFilterNet hat einen Rust-Kern. Er lässt sich als Crate direkt in ein GStreamer-Element einbauen (Muster `lzstrip`, §5) und braucht weder LADSPA noch Python.
2. **Qualität (Sekunden):** HTDemucs oder ein RoFormer-Modell auf GPU, segmentweise mit Überlappung. Das geht nur, wenn die ganze Ausgabe (Bild und Ton) um diese Zeit verzögert wird. Für YouTube oder Mediathek ist das hinnehmbar, für SRT-Rückwege nicht. Als eigener Modus zu kennzeichnen.
3. **Musik und Gesang erkennen:** Bei Gesang (Gottesdienst!) sollte die Klare-Sprache-Spur nicht absenken, so wie ARTE es nachgerüstet hat. Dafür braucht es einen Sprache-Musik-Klassifikator. Derselbe Baustein dient der Lautheitsregelung (§3.5).

### 1.3 Was lz-defeedback beiträgt

Geprüft am Repo `larszu/lz-defeedback` (privat, Stand v0.1.0, 11.10.2026):

| Baustein | Datei | Nutzen für lz-streamer |
|---|---|---|
| Notch-Bank: 24 Biquads, doppelte Genauigkeit, 0 ms Latenz, inaktive übersprungen | `src/dsp/NotchBank.h` | Muster für latenzfreie Filterketten (Brummfilter 50/100 Hz, Kerbfilter gegen Pfeiftöne im Saalton). Für die Dialogtrennung selbst nicht geeignet. |
| Eigene FFT (Radix-2, Größe frei, Vorgabe 4096) | `src/dsp/Fft.h` | Spektralanalyse ohne Fremdbibliothek, z. B. für eine Spektrumanzeige im Streamer. Rust hat mit `rustfft` aber Gleichwertiges [V]. |
| Spitzenanalyse: Schmalbandigkeit, Abstand zum Gesamtpegel, Stabilität über Takte, Obertonreihen-Test | `src/dsp/FeedbackDetector.h` | Methodik für „ist das tonal/Musik?“. Der Obertontest ist ein einfacher Gesangs- oder Instrumentenhinweis für den Sprache-Musik-Schalter [V]. |
| DSP-Kern **ohne JUCE** | `src/dsp/*.h` | Lässt sich per `cc`-Crate und kleiner C-Schnittstelle in Rust einbinden oder direkt portieren. Keine AGPL-Frage, weil JUCE nicht gebraucht wird. |
| Plugin als VST3/AU/Standalone (JUCE 8) | `CMakeLists.txt`, `src/plugin/*` | **Erstes Testobjekt für den Plugin-Host** in lz-streamer (Issue P1): Ein selbst gebautes Plugin zeigt, ob Laden, Parameter, Zustand und Latenzmeldung funktionieren. |
| Simulierter geschlossener Regelkreis als Test | `tests/sim_test.cpp` | Vorbild für einen Test der Lautheitsregelung mit synthetischen Programmen (Sprache, Musik, Pause, Werbesprung). |

Die Roadmap von lz-defeedback plant ein eigenes kleines Netz (< 20k Parameter, Gewichte als C++-Header, Inferenz handgeschrieben). Dasselbe Vorgehen eignet sich für einen **Sprache-Musik-Pause-Klassifikator** in lz-streamer: klein, deterministisch, ohne Laufzeitbibliothek [V].

**Lizenz:** JUCE 8 steht unter AGPLv3 oder einer kommerziellen Lizenz (README lz-defeedback). Der DSP-Kern ist davon unabhängig.

---

## 2. Plugins in einer Rust+GStreamer-Pipeline hosten

### 2.1 Wege

| Weg | Stand | Latenz und Echtzeit | Lizenz | Bewertung |
|---|---|---|---|---|
| **GStreamer `lv2`** (gst-plugins-bad, über lilv) | Jedes LV2-Plugin wird ein eigenes Element, benannt nach URI, z. B. `lsp-plug-in-plugins-lv2-compressor-stereo` ([GStreamer-Doku lv2](https://gstreamer.freedesktop.org/documentation/lv2/index.html)) | läuft im Streaming-Thread; Latenzmeldung des Plugins in der Doku nicht beschrieben [V] | LGPL (GStreamer), Plugins je eigene | **In der Homebrew-Installation nicht vorhanden** (`gst-inspect-1.0 lv2` leer) [L]. Unter Linux mit Distributionspaketen nutzbar [V]. Kein Absturzschutz. |
| **GStreamer `ladspa`** | dito für LADSPA, z. B. DeepFilterNet-LADSPA | dito | dito | lokal ebenfalls nicht vorhanden [L] |
| **Eigener VST3-Host** in Rust (Bindings, z. B. `vst3`-Crate aus coupler-rs) | VST3 SDK 3.8 seit 29.10.2025 unter **MIT** [H] ([Steinberg-Pressemitteilung](https://ocl-steinberg-live.steinberg.net/_storage/asset/819253/storage/master/Press%20Release%20-%202025-10-29%20-%20VST%203.8%20-%20EN.pdf), [Sonicstate](https://sonicstate.com/news/2025/10/30/vst-3-now-available-under-mit-license)) | Plugin meldet `getLatencySamples`; Prozess-Callback muss frei von Allokationen und Locks sein [V] | MIT (VST3). VST2 bleibt eigene Vereinbarung, ASIO-SDK separat (GPLv3 genannt) | **empfohlen** für Windows, Mac und Linux; Crate-Reife nicht geprüft [V] |
| **CLAP über `clack-host`** | sichere Rust-Hülle um `clap-sys`, mit Host-Beispiel (CPAL) | CLAP hat Latenz- und Thread-Modell eingebaut [V] | MIT/Apache-2.0, Rust ≥ 1.85 ([docs.rs clack-host](https://docs.rs/crate/clack-host/0.2.0), [GitHub prokopyl/clack](https://www.github.com/prokopyl/clack)) | gut. Laut Autor gibt es keine höherstufige Alternative [H]. Weniger Broadcast-Plugins als VST3 [V]. |
| **AU am Mac** über AVFoundation/AudioToolbox (`AVAudioUnit`, `AUAudioUnit`) | System-API, AUv2 und AUv3 | Render-Block in Echtzeit | Apple-SDK | nur Mac. Über `objc2`-Bindings aus Rust ansprechbar [V]. Für iZotope, Waves und NUGEN am Mac nötig, weil manche nur AU/VST3 liefern. |
| **Carla als Hilfsprozess** | Host für LV2, LADSPA, VST2, VST3, AU, SF2; mit OSC- und Bridge-Modus | +1 Puffer durch JACK oder die Bridge [V] | GPL-2.0+ [V] | als externer Prozess lizenzrechtlich unkritisch, aber zusätzlich JACK oder PipeWire nötig. Für Linux ein schneller Weg [V]. |
| **JUCE-Host als Hilfsprozess** | `AudioPluginHost`-Muster, alle Formate | dito | AGPLv3 oder kommerziell | nur sinnvoll, wenn JUCE ohnehin lizenziert ist. AGPL ist bei Weitergabe eines eigenen Hilfsprogramms zu klären [V]. |

### 2.2 Absturzschutz und Echtzeit (Entwurf) [V]

- **Plugin-Prozess getrennt vom Kanalprozess**, wie beim heutigen Aufpasser (`src/supervisor.rs`):
  - Ton per Shared-Memory-Ringpuffer (mmap, lock-free SPSC) hin und zurück.
  - Steuerung über JSON-Zeilen, wie beim Kanalprozess.
- **Latenz:** ein Block zusätzlich, bei 256 Samples und 48 kHz also 5,3 ms. Dazu kommt die vom Plugin gemeldete Latenz. Beides geht als `latency`-Antwort in die Pipeline-Latenzabfrage und hält A/V-Sync.
- **Ausfall:**
  - Meldet sich der Plugin-Prozess nicht innerhalb eines Blocks, schaltet das Element sofort auf Durchreichen (Bypass) und hält das Signal ohne Lücke.
  - Danach startet der Aufpasser den Prozess neu.
  - Das entspricht dem Standbild-Prinzip (`fallbackswitch`) für Ton.
- **Plugin-Zustand** (VST3-State-Chunk, CLAP-State) wird im Profil gespeichert, damit Neustarts denselben Klang liefern.
- **Keine Plugin-Oberfläche im Kanalprozess.** Bearbeitung entweder im Hilfsprozess-Fenster (nur lokal) oder über die Parameterliste in der Weboberfläche. Für Fernbedienung ist das der Normalfall.

### 2.3 Lizenzlage kurz

- **VST3:** MIT seit SDK 3.8 [H]. Der frühere Zwang zu GPLv3 oder Steinberg-Vertrag entfällt für neue Hosts. „VST“ als Marke bzw. Logo hat eigene Regeln [V, nicht geprüft].
- **CLAP:** MIT [V, Spezifikation nicht nachgelesen]. `clack`: MIT/Apache.
- **LV2:** ISC [V]. Plugins wie LSP sind LGPL, x42 und Calf GPL [V]. Für den Host ist das unkritisch, solange die Plugins getrennt geladen werden.

---

## 3. Jünger Audio D\*AP8 und Nachfolger, Level Magic, Wettbewerb

### 3.1 Produktlinie (heute bei Telos Alliance)

| Gerät | Funktion | Quelle |
|---|---|---|
| D\*AP8 FLX / TAP / CODEC Edition | 8-Kanal-Surround-Prozessor; Lizenzen getrennt: Level Magic, Fail-Over + automatisches Upmix | [Telos D\*AP8 FLX](https://telosalliance.com/on-air-tv-audio-processing/tv-loudness-control/junger-dap8-flx), [BSW](https://bswusa.com/junger-audio-d-ap8-flx/) [H/D] |
| D\*AP8 MAP | Monitoring-Prozessor: 8-Kanal-Monitor, Echtzeit-Metadaten-Emulation, volle Lautheitsmessung; Dolby-Toolkits als Option; ersetzt laut Hersteller bis zu sieben abgekündigte Dolby-Geräte | [Mix](https://www.mixonline.com/?p=4091) [H] |
| AIXpressor / flexAIserver (Software flexAI) | Level Magic, Sprachverarbeitung, Voice-over-Mischer (Stereo und 5.1) mit Mix-Minus, Upmix. 2023-12r4: verbesserter Voice-over-Automixer, neuer Upmixer mit Center-Ersatz | [Telos flexAI 2023-12r4](https://telosalliance.com/junger-audio-flexai-software-aixpressor-flexaiserver) [H] |
| Level Magic Upmix | Mono/Stereo → 5.1 aus Raumanteil und Spektralverteilung; „Surround Checker“ erkennt das Eingangsformat | [TV Technology](https://www.tvtechnology.com/news/jnger-audio-introduces-upmix-function-for-level-magic) [H] |
| Level Magic für MPEG-H | „selektive“ Lautheitsverarbeitung für MPEG-H TV Audio (Fraunhofer-Kooperation 2015) | [Fraunhofer IIS 2015](https://www.iis.fraunhofer.de/en/pr/2015/20150714_ame_MPEGHAA-Juenger.html) [H] |
| Level Magic II | Konformität EBU R128, ITU-R BS.1770 (alle Fassungen), ATSC A/85, ARIB TR-B32 [H] | Telos-/Jünger-Broschüre laut Suchergebnis; Broschüren-PDF lieferte beim Abruf nur HTML |
| Plugin „Level Magic“ (mit Flux, 2015) | Software-Version als Plugin | [Production Expert](https://www.production-expert.com/home-page/2015/1/23/flux-junger-collaborate-to-produce-level-magic-loudness-proc.html) [D] |

**Preise:** in dieser Runde nicht gefunden. Die Händlersuche für AIXpressor lieferte nichts Verwertbares. Lizenzen werden einzeln verkauft, z. B. „Level Magic License für D\*AP4“ ([Dale Pro Audio](https://daleproaudio.com/products/junger-l_d-ap4_lm-level-magic-license-for-d-ap4)) [D].

### 3.2 Wie Level Magic arbeitet, laut Handbuch

Quelle ist die Bedienungsanleitung des Jünger D07, nach Seiten. Wiedergegeben ist nur die Funktionsweise, in eigenen Worten.

- **Drei Stufen gleichzeitig:** adaptive AGC (langsam), Transientenprozessor (schnell) und Brickwall-Limiter. Der Limiter arbeitet unabhängig vom Leveller und regelt nur die Spitze ([S. 45](https://www.manualslib.com/manual/803625/Junger-D07.html?page=45); [TV Technology](https://www.tvtechnology.com/news/jnger-audio-upgrades-level-magic-adaptive-loudness-control-system)).
- **Zwei Detektoren:** Ein Quasi-Spitzenwert-Detektor (QP) und ein RMS-Detektor laufen parallel. Ihre Differenz zeigt, wie viel Transientenanteil gerade kommt. Danach wird die nötige Verstärkungsänderung zwischen Transientenprozessor und AGC aufgeteilt. Die Gesamtverstärkung ist die Summe beider ([S. 45](https://www.manualslib.com/manual/803625/Junger-D07.html?page=45)).
- **Hörbarkeitsgrenze:** Die AGC soll sich höchstens um etwa 1 dB je 5 s ändern, damit die Regelung praktisch unhörbar bleibt (ebd.).
- **Zwei Bezugspegel:**
  - „Operating Level“ steuert AGC und Transientenprozessor.
  - „Peak Level“ steuert den Limiter.
  - Empfohlen sind 6–9 dB Abstand, nicht über etwa 10 dB ([S. 7](https://www.manualslib.com/manual/803625/Junger-D07.html?page=7), [S. 47](https://www.manualslib.com/manual/803625/Junger-D07.html?page=47)).
- **Adaptive Zeitkonstanten:** Attack und Release passen sich dem Signal an („Adaptive Dynamic Range Control“). Das Ganze nennt Jünger „Multi-Loop“-Prinzip ([S. 7](https://www.manualslib.com/manual/803625/Junger-D07.html?page=7)).
- **Lookahead:** etwa 2 ms Signalverzögerung. Die Regelung sieht so die Spitze voraus und verhindert Überschwingen. Ein unverzögertes Direktsignal muss um dieselben 2 ms verzögert werden, sonst entsteht ein Kammfilter ([S. 34](https://www.manualslib.com/manual/803625/Junger-D07.html?page=34)).
- **Voreinstellungen „soft/mid/hard“:** Sie verschieben Bereich und Zeit von AGC und Transientenprozessor. „Hard“ ist für Publikum und sprachlastige Sendungen gedacht, „soft“ für Film und Klassik ([S. 47](https://www.manualslib.com/manual/803625/Junger-D07.html?page=47); Tabellenzuordnung aus dem Textauszug nur ungefähr). Laut Suchauszug hat „soft“ einen AGC-Bereich von 15 dB bei mindestens 2 min Zeitkonstante [D, nicht selbst nachgelesen].
- **Pausen (Gate/Freeze):** In den gelesenen Handbuchseiten nicht beschrieben. Üblich, und für Level Magic zu vermuten, ist ein Einfrieren der AGC unterhalb einer Schwelle, damit Pausen und Raumrauschen nicht hochgezogen werden [V].
- **Sprache oder Musik:** Eine Sprach-/Musikunterscheidung ist für Level Magic nicht belegt. Belegt ist sie bei Dolby (Dialogue Intelligence) und Auphonic (§3.4).

### 3.3 Patente

- Gesucht wurde nach Jünger Audio als Anmelder und nach Level Magic, Loudness und AGC, auf Englisch und Deutsch. **Kein Patent von Jünger Audio wurde gefunden.**
- Gefundene Lautheitspatente gehören anderen, etwa Harman Becker („Automatic loudness control“, [US 2014/0037108](https://patents.justia.com/patent/20140037108)). Bei EP 2367286 („Automatic correction of loudness level in audio signals“) blieb der Anmelder ungeklärt, die EPO-Seite war nicht abrufbar.
- **Folgerung [V]:** Level Magic ist entweder nicht oder nicht unter diesem Namen patentiert. Es beruht auf allgemein bekannten Prinzipien: AGC, Transientenlimiter, Lookahead-Limiter und Detektorvergleich. Ein eigener Nachbau nach diesen Prinzipien ist unproblematisch. Vor einem Vertrieb ist trotzdem eine kurze Recherche in DEPATISnet bzw. Espacenet nach „Jünger“ als Anmelder sinnvoll, weil die Websuche Patentdatenbanken nur lückenhaft abdeckt.

### 3.4 Wettbewerb und offene Bausteine

| Anbieter / Baustein | Was | Live-tauglich? | Quelle |
|---|---|---|---|
| Dolby Dialogue Intelligence (LM100, Media Meter 2, DP600) | misst die Lautheit nur, wenn Dialog vorhanden ist. Referenzcode **kostenlos**, optimierter Code kostenpflichtig | Messung ja | [Dolby Speech-Gating-Referenzcode](https://professional.dolby.com/support/dolbys-speech-gating-reference-code/) [H] |
| Linear Acoustic (Telos) AERO, LA-5291 | TV-Lautheitsprozessoren, Upmix | ja | nur Vergleichsblatt LA-5291 vs. Dolby DP591 gefunden ([Telos](https://telosalliance.com/uploads/LA%20Product%20Photos/LA-5291/Telos%20Allaince_Linear%20Acoustic%20LA-5291%20vs%20Dolby%C2%AE%20DP591_Comparison%20Sheet.pdf)); Funktionen nicht weiter belegt |
| Orban Optimod (TV/Surround), RTW, TC Electronic DB4/DB6 | Lautheitsprozessoren bzw. Messung | ja | in dieser Runde nicht belegt |
| Auphonic Adaptive Leveler | klassifiziert per Machine Learning Sprache, Musik und Hintergrund; Sprache wird stärker angeglichen, Musik schonender, Hintergrund weniger angehoben | **offline** (Batch); Desktop-Leveler läuft nicht ab macOS Ventura | [Auphonic Leveler](https://auphonic.com/help/desktop/leveler.html), [Beispiele](https://auphonic.com/blog/2013/02/28/audio-examples-auphonic-algorithms/) [H] |
| iZotope RX/Ozone, NUGEN, Waves WLM | Plugins für Messung und Korrektur | über Plugin-Host (§2) nutzbar | nicht im Einzelnen geprüft |
| ffmpeg `loudnorm` | EBU R128, Modi dynamisch und linear; dynamisch mit Upsampling auf 192 kHz | Einzeldurchgang für Livestreams möglich, aber mit 3 s Lookahead (siehe GStreamer-Port) | [ffmpeg-filters](https://ffmpeg.org/ffmpeg-filters.html#loudnorm) |
| GStreamer **`audioloudnorm`** (gst-plugins-rs) | Port von `af_loudnorm.c`; Vorgaben −24 LUFS, LRA 7, −2 dBTP | **nein für < 50 ms:** `GAIN_LOOKAHEAD = 3 s`, Rahmen 100 ms, Limiter-Lookahead 10 ms | [Quelltext imp.rs](https://gitlab.freedesktop.org/gstreamer/gst-plugins-rs/-/raw/main/audio/audiofx/src/audioloudnorm/imp.rs) [L] |
| GStreamer **`ebur128level`** | Lautheitsmessung M/S/I/LRA/True Peak als Bus-Nachricht | ja (Messung) | [L] |
| ffmpeg `dynaudnorm` | gleicht Abschnitte auf Spitzenwert an, ohne Kompression im Abschnitt; Rahmen 10–8000 ms, Vorgabe 500 ms, Gauß-Glättung über mehrere Rahmen | Verzögerung mehrere Sekunden [V] | [ffmpeg-filters](https://ffmpeg.org/ffmpeg-filters.html#dynaudnorm) |
| ffmpeg `speechnorm` | dehnt oder staucht jede Halbwelle (zwischen zwei Nulldurchgängen) auf einen Zielspitzenwert, Faktoren begrenzt | sehr latenzarm, aber Spitzen- statt Lautheitsbezug; verfärbt [V] | [ffmpeg-filters](https://ffmpeg.org/ffmpeg-filters.html#speechnorm) |
| libebur128 / Rust-Crate `ebur128` | Referenzmessung BS.1770 | ja | lz-scopes nutzt die Filterformel aus libebur128 (`src/audio/dsp/kweight.ts`, MIT) |
| LSP-Plugins, x42, Calf | Kompressor, Limiter, Multiband, EQ (LV2/VST) | ja, über Host | GStreamer-Doku `lv2` nennt LSP-Elemente |

### 3.5 Was lz-streamer für eine eigene, latenzarme Lautheitsregelung bauen muss [V]

Ein eigenes Rust-Element `lzloudness` nach dem Muster `lzstrip` aus lz-vision-mixer:

1. **Messung** nach BS.1770-5 (K-Filter, M 400 ms, S 3 s, I gated, LRA, True Peak 4×). Referenz ist `lz-scopes/src/audio/dsp/{kweight,loudness,lra,truepeak}.ts` mit Tests gegen EBU Tech 3341/3342. In Rust über `ebur128`-Crate oder eigener Port, die Testfälle aus lz-scopes werden übernommen.
2. **Langsame AGC** auf eine Ziel-Lautheit (Profile: R128 −23 LUFS, A/85 −24 LKFS, Streaming −16/−14 LUFS).
   - Stellgeschwindigkeit ≤ 1 dB/5 s, angelehnt an das Jünger-Handbuch.
   - Bereich ±12–15 dB.
   - Regelgröße ist die Short-Term-Lautheit, mit Sprachgewichtung, wenn Sprache erkannt ist.
3. **Einfrieren bei Pause:** unter einer Schwelle (z. B. −45 LUFS Momentary) oder ohne Sprach-/Musikaktivität hält die AGC ihren Wert.
4. **Schneller Transientenprozessor:**
   - Breitband, oder 3–4 Bänder mit Linkwitz-Riley-Weiche (Mehrband als Option).
   - Der Anteil wird aus der Differenz QP−RMS gesteuert, das Gesamtziel ist das Operating Level.
5. **True-Peak-Limiter** mit 1–2 ms Lookahead, Ziel −1 dBTP (Streaming) bzw. −2 dBTP.
6. **Sprache/Musik/Pause:** zuerst ein einfacher VAD, z. B. Silero VAD (MIT, [GitHub snakers4/silero-vad](https://github.com/snakers4/silero-vad)) oder RNNoise-VAD-Wahrscheinlichkeit. Später ein eigener kleiner Klassifikator wie in der lz-defeedback-Roadmap. Damit lässt sich „Dialog-gated“ messen wie bei Dolby DI.
7. **Gesamtlatenz** ≤ 5 ms (Lookahead), korrekt als Element-Latenz gemeldet.
8. **Anzeige und Protokoll:**
   - Pegel/Lautheit je Spur in der Weboberfläche: M/S/I, LRA, TP und Regelhub.
   - Neue Alarme: „Lautheit außerhalb Ziel“, „Stille“, „Übersteuerung“, eingehängt in die vorhandenen Alarmschwellen (`src/ops.rs`).
   - Lautheitsbericht im CSV-Bericht.
9. **Upmix und Downmix:** Downmix ist vorhanden (Matrix in `src/audio.rs`). Upmix auf 5.1 ist für die Zielgruppe nachrangig.

---

## 4. Untertitel, alle Wege

### 4.1 Fraunhofer IAIS Live-Subtitling und Audio Mining

- **Einsatz:**
  - Echtzeit-Transkription von Interviews und Reden. Live-Untertitel für eine deutsche öffentlich-rechtliche Anstalt und für Debatten im Sächsischen Landtag [H] ([IBC 2022](https://www.digitalmedia.fraunhofer.de/en/veranstaltungen/ibc-2022/audio-mining-live-subtitling.html)).
  - Audio Mining analysiert bei einer großen deutschen Anstalt etwa 2000 h täglich, auch das ZDF nutzt es [H] ([Trendbroschüre 2021](https://www.digitalmedia.fraunhofer.de/en/mediainformation/trendbrochures/trendbrochure-2021/audio-mining-live-subtitling.html)).
  - Hintergrund: Online-Erkennung mit Sprecherzuordnung aus dem Projekt „Live-Caption“, auch für bairischen Dialekt ([Interspeech 2015](https://www.isca-archive.org/interspeech_2015/stadtschnitzer15_interspeech.html)).
  - Sprachen Deutsch und Englisch laut IBC-2019-Seite ([IBC 2019](https://www.digitalmedia.fraunhofer.de/en/veranstaltungen/ibc-2019/live_subtitling_2019.html)).
- **API, Lizenz, Preise, Self-Hosting:** **öffentlich nicht dokumentiert.** Kontakt läuft über den Geschäftsbereich Digital Media (contact-digitalmedia@iis.fraunhofer.de laut IBC-2022-Seite).
- **Für Lars [V]:** nur als Lizenzprojekt mit Angebot, eher für Behörden- oder Rundfunkkunden. Für Kirche, Messe und Firmenevent sind offene Modelle oder Cloud-Dienste mit Minutenpreis näherliegend. Lohnt eine Anfrage, wenn ein Landtag oder eine Behörde als Kunde ansteht. Fraunhofer hat dort Referenzen.

### 4.2 Offene Live-Spracherkennung und Übersetzung

| Engine | Deutsch | Latenz | Lizenz | In GStreamer | Quelle |
|---|---|---|---|---|---|
| **whisper.cpp** über `whispertranscriber` | ja (multilingual), Übersetzung nur nach Englisch | stellbar: `chunk-duration`, `live-edge-offset`, `latency` zählen zur Gesamtlatenz; GPU wählbar | MIT (whisper.cpp), Element MPL-2.0 | **ja** [L] | [docs.rs gst-plugin-whisper](https://docs.rs/crate/gst-plugin-whisper/latest/source/src/lib.rs) |
| whisper_streaming (UFAL) | ja | gemessen **4,4 s** im Mittel für Deutsch (Englisch 3,3 s), A40-GPU, MinChunk 1 s | MIT [V] | nein (Python) | [arXiv 2307.14743](https://arxiv.org/html/2307.14743) |
| WhisperLive (Collabora) | ja | „nahezu live“, mit VAD; keine Zahl gefunden | MIT [V] | nein (Server, faster-whisper/TensorRT/OpenVINO) | [GitHub collabora/WhisperLive](https://awesome.ecosyste.ms/projects/github.com%2Fcollabora%2Fwhisperlive) |
| **NVIDIA Parakeet-TDT-0.6B-v3 / Canary-1B-v2** | 25 europäische Sprachen inkl. Deutsch; Canary auch Übersetzung | chunk-/batchweise, 2–7 GB, GPU; keine deutsche Streaming-Latenz gefunden | CC-BY-4.0 [V] | nein (NeMo/Riva) | [arXiv 2509.14128](https://arxiv.org/abs/2509.14128v2) |
| **Vosk** | deutsches Modell vorhanden [V] | gering, Wort für Wort [V] | Apache-2.0 [V] | nein | im Broadcast-intercom bereits eingebaut (§5) |
| Moonshine | vor allem Englisch [V] | sehr gering [V] | MIT [V] | nein | nicht geprüft |
| Speechmatics (Cloud oder Container) | ja, 55+ Sprachen [D] | nicht gefunden | kommerziell; Echtzeit ca. 0,24–1,04 $/h laut Aggregatoren (widersprüchlich), Container nur auf Anfrage | **ja** `speechmaticstranscriber` [L] | [Speechmatics Container-Doku](https://docs.speechmatics.com/deployments/container/performance-and-cost), [costbench](https://costbench.com/software/ai-transcription-apis/speechmatics/) [D] |
| Deepgram, AWS Transcribe/Translate | ja [V] | – | kommerziell | **ja** `deepgramtranscriber`, `awstranscriber`, `awstranslate` [L] | – |

**Einschätzung [V]:**
- Mit Whisper liegt Deutsch live realistisch bei 2–5 s Textverzug. Fernsehuntertitel mit Respeaking liegen in ähnlicher Größenordnung, für Gottesdienst und Firmenevent ist das brauchbar.
- Für unter 1 s braucht es ein Streaming-Modell (Vosk, Parakeet-Streaming-Varianten) und weniger Genauigkeit.
- `transcriberbin` kapselt Engine-Wahl, Puffer und Ausgabeformat. Die Engine sollte im Profil umschaltbar sein: lokal Whisper oder Vosk, Cloud mit eigenem Schlüssel.

### 4.3 Ausgabewege

| Weg | Wofür | Bausteine | Status |
|---|---|---|---|
| **CEA-608/708 im Videostrom** (H.264/H.265-SEI) | YouTube (Einstellung „Embedded 608/708“), Facebook, Fernseher, Haivision-/Videon-Decoder, SRT-Rückweg | `tttocea608`/`tttocea708` → `h264ccinserter`/`h265ccinserter` oder `cccombiner`; `cea608mux`, `ccconverter` | alle vorhanden [L]. YouTube nimmt nur **eine** Spur an, obwohl 608/708 bis zu 4 Sprachen trägt ([Google](https://support.google.com/youtube/answer/3068031?hl=en)) |
| **Durchreichen aus SDI** | Untertitel, die schon im Eingang sind (Fahrplan Punkt 3) | `decklinkvideosrc output-cc=true` → GstMeta → `h264ccinserter` | vorhanden [L]. Haivision Makito X4 macht genau das: CTA-608/708 aus dem VANC (SMPTE 334-1/-2, CDP) in den Video-ES des TS ([Haivision-Doku](https://doc.haivision.com/MakitoX4Enc/1.9/closed-captioning)) [H] |
| **Zurück in SDI-VANC** (Decoder, ST 334) | Decoder-Ausgang an Kreuzschiene oder Sender | `decklinkvideosink cc-line=<Zeile>`; `h264ccextractor` am Decoder | vorhanden [L] |
| **ST 2038 im TS** | allgemeines Ancillary (auch SCTE-104, AFD) | `cctost2038anc`, `st2038ancmux`, `st2038combiner` | vorhanden [L]. Gehört zu Fahrplan Punkt 3 |
| **Eingebrannt, schlicht** | Notfall- oder Messeanzeige ohne Grafik-App | `cea608overlay`/`cea708overlay` (rendert die CC-Meta direkt), `textoverlay` | vorhanden [L]. Falle aus lz-vision-mixer: `textoverlay` skaliert die Schrift mit der Bildbreite |
| **Eingebrannt als OGraf-Grafik** | gestaltete Untertitel in LZM-Optik, auch Liedtexte und Bauchbinden | OGraf v1 ist stabil, die Control-API ist Entwurf (Abschluss Mitte 2026 erwartet) ([ograf.ebu.io](https://ograf.ebu.io), [GitHub ebu/ograf](https://gh.r-mj.com/ebu/ograf)). Renderer aus lz-vision-mixer (`renderer/`, Electron, sendet RGBA-Bilder je Ebene per TCP) + Grafik `renderer/graphics/lz-captions` | `wpesrc`/`cefsrc` fehlen lokal [L]. Darum den Renderer aus dem Mischer übernehmen statt eines Browsers in der Pipeline |
| **DVB-Subtitles im TS** | IPTV/Multicast an Set-Top-Boxen und Fernseher (z. B. lz-media-station-Kanal) | `dvbsubenc` (Bitmap aus Text-Overlay), `mpegtsmux` nimmt `subpicture/x-dvb` | vorhanden [L] |
| **Teletext (Seite 777/150)** | ältere TV-Empfänger | kein Encoder in GStreamer gefunden (`teletextdec` fehlt lokal, Encoder gibt es nicht) [L/V] | nur mit eigenem PES-Encoder. Niedrige Priorität |
| **WebVTT / TTML / EBU-TT-D** | HLS/DASH, Mediathek, Aufnahme-Nachbereitung | `webvttenc`, `jsontovtt`, `cea608tott` (→ SRT/VTT), `ttmlparse`/`ttmlrender`, `hlscmafsink`/`hlssink3` | Elemente vorhanden [L]. Ob `hlscmafsink` eine Untertitel-Rendition anlegt, ist offen [V]. Hängt am Fahrplan Punkt 5 (HLS) |
| **Untertiteldatei zur Aufnahme** | Archiv, YouTube-Nachbearbeitung | `.srt`/`.vtt` neben TS/MP4; `sccenc`, `mccenc` für Broadcast-Lieferung | vorhanden [L] |
| **YouTube HTTP POST** | wenn kein 608 im Strom möglich ist | signierte Ingest-URL, eine Untertitelquelle je Eingang; **verlangt 30 oder 60 s Sendeverzögerung** ([Google](https://support.google.com/youtube/answer/3068031?hl=en)). Format laut Drittbibliothek: `text/plain`, Parameter `seq`, Zeitstempel je Zeile ([PyPI lcyt](https://pypi.org/project/lcyt/)) [D] | eigener HTTP-Client nötig. Nachrangig, weil 608 einfacher ist |
| **Untertitel über SRT** | – | SRT überträgt den TS, Untertitel stecken darin als 608-SEI, DVB-PID oder ST 2038. Eigene SRT-Untertitel-Metadaten gibt es nicht [V] | – |

**Mindestumfang für lz-streamer:**
1. 608/708 im H.264/H.265 (deckt YouTube, SRT, Decoder und TV ab)
2. SDI-Durchreichen und Rückgabe in VANC
3. OGraf-Einbrennen
4. Untertiteldatei zur Aufnahme
5. WebVTT, sobald HLS kommt
6. DVB-Sub für IPTV als Option
7. YouTube-POST und Teletext nur auf Nachfrage

---

## 5. Was in Lars' Repos schon da ist

| Repo | Baustein | Datei(en) | Verwendung in lz-streamer |
|---|---|---|---|
| **lz-vision-mixer** (privat) | eigenes GStreamer-Element in Rust: Kanalzug mit RNNoise (`audiornnoise`), Hochpass, Gate mit Hold, Kompressor (Soft Knee), Make-up | `engine/src/dsp.rs`, `engine/src/strip_element.rs` (`lzstrip`, Registrierung nur im Programm) | **Vorlage** für `lzloudness`, `lzclearspeech` und das Plugin-Host-Element. Den Kanalzug direkt für Mikrofoneingänge übernehmen |
| lz-vision-mixer | OGraf-Renderer (Electron) mit Ebenen, sendet Rohbilder je Ebene per TCP, bei Rückstau gewinnt das neueste Bild | `renderer/src/main/{frameSink,protocol,layer,engineLink}.ts`, Protokoll in `docs/api.md` | **als Grafik-Hilfsprozess wiederverwenden**; lz-streamer bekommt einen Bildeingang wie die Mischer-Engine |
| lz-vision-mixer | OGraf-Grafiken `lz-captions` (2 Zeilen, Überblendung, Auto-Ausblenden, oben/unten), `lz-lower-third`, `lz-bug`, `lz-title`, `lz-lyrics` | `renderer/graphics/*` | Untertitel-Einbrennen ohne neue Grafikarbeit |
| lz-vision-mixer | Untertitel per OSC `/lzvm/caption` aus dem Intercom | `engine/src/osc.rs` | gleiche Schnittstelle als „externe Untertitelquelle“ |
| lz-vision-mixer | **GStreamer-Fallen** | Memory/README | übernehmen, siehe Liste unten |
| **lz-scopes** (öffentlich) | Lautheit BS.1770-5/EBU Tech 3341, LRA Tech 3342, True Peak 4×, K-Filter (Formel aus libebur128) | `src/audio/dsp/{loudness,lra,truepeak,kweight,meters}.ts`, Tests `test/audio.test.ts` | Referenz und Testfälle für die Rust-Messung |
| lz-scopes | A/V-Versatz (Blitz und Piep), Latenzstempel | `src/audio/dsp/avsync.ts`, `src/latency.ts` | Abnahme: Untertitelverzug und Klare-Sprache-Spur lippensynchron |
| lz-scopes | VANC-Parser: erkennt CEA-708/608 (DID 0x61, SDID 0x01/0x02), ST 352, ST 2108, ATC, AFD | `src/vanc.ts`, `test/vanc.test.ts` | Eingangsanzeige „CC vorhanden“ und Abnahme des Durchreichens |
| lz-scopes | Steuer-API und Companion-Muster | `docs/control-api.md` | Companion-Tasten: Untertitel an/aus, Klare Sprache an/aus |
| **lz-presenter** (privat) | OGraf-Grafik für Liedtexte, schlanker OGraf-Renderer | `graphic/lyrics.ograf.json`, `graphic/graphic.mjs`, `public/output.html` | Liedtexte im Stream über denselben Grafikweg. Geplant ist automatisches Weiterschalten per Spracherkennung (`docs/funktionsplan.md` §3) → gemeinsame ASR-Engine |
| **Broadcast-intercom** (privat) | Vosk-Transkription je Kanal, Modell-Download per API | `apps/server/src/index.ts`, `/api/transcription/*`, `VOSK_MODEL_PATH` | Vosk-Erfahrung; Intercom als Untertitelquelle |
| **lz-defeedback** (privat) | siehe §1.3 | `src/dsp/*`, `tests/sim_test.cpp` | Testobjekt für den Plugin-Host, DSP-Muster |
| **lz-camera-bridge** (privat) | Companion-Modul-Paket, NMOS-Wissen; seit 04.11.2026 gilt für Bridge-Geräte NMOS (IS-12/MS-05-02) und je Gerät ein Companion-Modul | `packages/companion-module-lz-camera-bridge`, `docs/wissen/nmos.md`, `docs/ressourcenmodell.md` | lz-streamer als Gerät der Bridge (Fahrplan Punkt 9). Untertitel- und Audio-Parameter als steuerbare Eigenschaften gleich NMOS-tauglich benennen |

**GStreamer-Fallen aus lz-vision-mixer**, die hier zutreffen:
- `interaudiosrc` puffert per Vorgabe 1 s und verbraucht Samples. Zwei Leser bekommen je 35–40 % Stille, darum braucht jeder Ausgang einen eigenen Abgriff.
- `gst audiodynamic` ist nur ein Waveshaper und für Sprache ungeeignet.
- Dynamische Zweige erst nach PLAYING anhängen. Mixer-Pads vor dem Stoppen eines Bins freigeben.
- `textoverlay` skaliert die Schrift mit der Bildbreite.
- Ein verstecktes, undurchsichtiges Offscreen-Fenster im Renderer malt nicht (`transparent: true` setzen).

**Offene Projekte auf GitHub**, die Teile abdecken:
- Wowza-Modul Whisper → WebVTT/608: Quelle offen, braucht aber Wowza ([Wowza-Doku](https://www.wowza.com/docs/convert-speech-to-text-to-generate-live-stream-captions-with-openai-whisper)).
- WhisperLive: Server mit RTSP/HLS-Eingang.
- `ograf-core`: Rust-Crate für die OGraf Control-API, Entwurf ([docs.rs](https://docs.rs/crate/ograf-core/0.3.0/source/README.md)).
- EBU „OGraf Simple Rendering System“, Browser-Renderer im Repo `ebu/ograf`.
- Zero Density „OGraf Studio“: Editor, kein Renderer ([RedShark](https://www.redsharknews.com/zero-density-ograf-studio-open-source-graphics)).
- Ein fertiger offener Server für Live-Stream rein → 608 + VTT raus wurde nicht gefunden. GStreamer `transcriberbin` kommt dem am nächsten.

---

## 6. Markt: Encoder mit Audioverarbeitung und Live-Untertiteln

| Anbieter | Produkt | Audio | Live-Untertitel | Preis | Quelle |
|---|---|---|---|---|---|
| AI-Media | LEXI Text Encoder, LEXI Voice Encoder (NAB 2026, 1 HE, UHD592), ALTA RTMP (früher Falcon) | Voice: Übersetzung und Sprachausgabe | LEXI (KI) | Hardware als Abo (HaaS). Berichtet: Stufen 1200–3500 $/Monat [D]; LEXI Voice 30 $/h je Sprachpaar (Aussage des CEO) [D]; Analystenschätzung LEXI ≈ 10 $/h [D] | [AI-Media Falcon/ALTA](https://www.ai-media.tv/our-products/caption-delivery/ip-caption-encoders/falcon/), [Pulse](https://www.pulse.bot/hardware/news/nab-2026-ai-media-launches-lexi-text-encoder-and-lexi-voice-encoder-cb5132a4-eebf-41a9-bfce-739bfeeefa38/), [Substack-Analyse](https://walnutscapital.substack.com/p/ai-media-technologies-ltd-aimax) |
| ENCO | enCaption, Raptor (Cloud, IBC 2025/ISE 2026): 608 in SRT/RTMP/HLS-Ströme | – | KI | nicht gefunden | [ENCO Raptor](https://www.enco.com/blog/encos-raptor-sparks-ai-subtitling-in-the-cloud-at-ise-2026) [H] |
| SyncWords | Cloud, automatisch (KI) oder menschlich (CART), auch über SRT | Synchronisation | ja | nicht gefunden | [SyncWords](https://www.syncwords.com/products/automated-live-captions-using-srt-streaming) [H] |
| Haivision | Makito X4 | Durchreichen | **nur Durchreichen** 608/708 aus SDI | – | [Haivision-Doku](https://doc.haivision.com/MakitoX4Enc/1.9/closed-captioning) [H] |
| Videon | EdgeCaster / LiveEdge | – | Durchreichen 608/708 (SMPTE 334-2). KI-Container-Plattform, aber kein Untertitelmodul gefunden; EdgeCaster teils nicht mehr lieferbar | – | [Markertek](https://www.markertek.com/product/vci-edgecaster/videon-edgecaster-4k-hevc-encoder-with-adb-over-ip) [D] |
| Jünger / Telos | AIXpressor, D\*AP | Level Magic, Upmix, Voice-over | nein | auf Anfrage | §3.1 |
| Wowza | Streaming Engine + Whisper-Modul | – | Whisper → VTT/608 | Wowza-Lizenz | [Wowza-Doku](https://www.wowza.com/docs/convert-speech-to-text-to-generate-live-stream-captions-with-openai-whisper) |
| Speechmatics, Deepgram, Verbit | Spracherkennung als Dienst | – | Rohtext | Speechmatics ≈ 0,24–1,04 $/h [D] | §4.2 |
| AJA, Kiloview, Matrox, Appear, Ateme, Elemental | – | – | – | – | **in dieser Runde nicht belegt.** Die Suche fand für AJA, Kiloview und Matrox keine eingebaute KI-Untertitelung |

**Lücke und Alleinstellung [V]:**
- Kein gefundener Anbieter bündelt SRT/RTMP-Encoder, **lokale** deutsche Live-Untertitel (offline, ohne Datenabfluss), latenzarme Lautheitsregelung und eine **Klare-Sprache-Spur** in einem Gerät ohne Abo.
- Die Wettbewerber trennen das:
  - Encoder reichen nur durch (Haivision, Videon).
  - Untertitel-Encoder sind Abo-Hardware mit Cloud-Spracherkennung (AI-Media, ENCO).
  - Audioprozessoren haben weder Encoder noch Untertitel (Jünger).

**Zielgruppe** (Lars: kleine Produktionen, Kirchen, Firmenevents, Messen, Behörden):
- Kirchen: Gottesdienst-Stream mit Untertiteln und Liedtexten aus lz-presenter, Klare Sprache für Ältere.
- Behörden und Ratssitzungen: Untertitel, lokal und DSGVO-freundlich. Barrierefreiheitspflichten sind im Einzelfall zu prüfen [V].
- Firmen und Messen: Lautheit konstant über Redner, Einspieler und Musik; Untertitel als Bauchbinde im Messestand-Bildschirm.
- **Preisanker [V]:** Ein LEXI-Encoder im Abo kostet laut Bericht ab 1200 $/Monat. lz-streamer auf einem Mac mini oder Linux-PC mit lokalem Whisper hat keine laufenden Kosten. Das ist ein starkes Verkaufsargument für Dienstleistung statt Produktverkauf, und es passt zu „Produkte statt Pitch“.

---

## 7. Issue-Liste für lz-streamer

Reihenfolge grob von oben nach unten. „Abh.“ steht für Abhängigkeiten. Aufwand S/M/L wie in `funktionsparitaet-kb.md`.

1. **A1 – Tonpegel und Lautheit je Spur anzeigen (M)**
   - `ebur128level` hinter jede Tonspur, Werte M/S/I/LRA/TP in `/api/state` und als Balken in der Oberfläche.
   - Alarme „außerhalb Ziel“, „Stille > n s“, „TP > −1 dBTP“ an die vorhandenen Schwellen hängen, Lautheitsspalten in den CSV-Bericht.
   - **Abnahme:** EBU-Tech-3341-Testsignale (aus lz-scopes übernommen) ergeben ±0,1 LU; Integrationstest mit Testton −23 LUFS.
   - **Abh.:** keine.

2. **A2 – Eigenes Rust-Element `lzloudness`: latenzarme Lautheitsregelung (L)**
   - Nachbau des Prinzips aus §3.5:
     - langsame AGC (≤ 1 dB/5 s) auf ein Profilziel
     - Einfrieren in Pausen
     - Transientenprozessor gesteuert über die QP/RMS-Differenz
     - True-Peak-Limiter mit 1–2 ms Lookahead
   - Muster `lzstrip` aus lz-vision-mixer. Voreinstellungen soft/mid/hard, Ziele R128, A/85 und Streaming.
   - **Abnahme:**
     - Latenz ≤ 5 ms, als Element-Latenz gemeldet, A/V-Versatz unverändert (Messung mit `lz-streamer latenz`).
     - Programm aus Sprache −30/−18 LUFS im Wechsel, Musik und Pause: S-Lautheit bleibt in ±2 LU des Ziels, kein Hochziehen in Pausen, TP ≤ Ziel.
     - Unit-Tests wie `sim_test.cpp`.
   - **Abh.:** A1.

3. **A3 – Sprache/Musik/Pause-Erkennung (M)**
   - VAD (Silero oder RNNoise-Wahrscheinlichkeit) als gemeinsamer Dienst. Er steuert in A2 Dialog-Messung und Einfrieren und in K1 das Aussetzen bei Gesang.
   - Später eigener kleiner Klassifikator nach dem Plan in lz-defeedback.
   - **Abnahme:** Testkorpus (Sprache, Gesang, Orgel, Applaus, Stille) mit ≥ 90 % richtiger Klasse je 1-s-Fenster; Rechenzeit < 1 ms je 10 ms Audio am M1 Pro.
   - **Abh.:** keine.

4. **K1 – Klare-Sprache-Spur, latenzarm (L)**
   - DeepFilterNet (Rust-Crate) trennt Sprache und Rest. Ausgabe als zusätzliche Tonspur `s + g·r`, g einstellbar (Vorgabe −9 dB), danach `lzloudness`.
   - Spur im TS mit eigener Sprachkennung und Namen „Klare Sprache“. Bei erkanntem Gesang (A3) wird nicht abgesenkt.
   - **Abnahme:**
     - Zusatzlatenz ≤ 30 ms, Spur lippensynchron (lz-scopes-AV-Messung).
     - Hörtest-Protokoll mit 3 Saalaufnahmen.
     - Hauptspur bleibt bitgleich.
     - Rechenlast für 1080p50-Kanal + Spur am M1 Pro < 1 Kern zusätzlich.
   - **Abh.:** A2, A3; Tonspur-Matrix `src/audio.rs`.

5. **K2 – Klare Sprache, Qualitätsmodus mit Verzögerung (M, nachrangig)**
   - Trennmodell im GPU-Segmentbetrieb (HTDemucs-Fork oder RoFormer) mit einigen Sekunden Puffer. Die gesamte Ausgabe (Bild und alle Spuren) wird gleich verzögert.
   - Nur für Ziele, deren Latenz egal ist (YouTube, Aufnahme). Die Oberfläche zeigt die Verzögerung deutlich an.
   - **Abnahme:** Verzögerung konstant und angezeigt; A/V-Versatz < 20 ms; Vergleichsaufnahme K1 gegen K2.
   - **Abh.:** K1.

6. **P1 – Plugin-Host im eigenen Prozess, VST3 zuerst (L)**
   - Ein Element `lzplugin` reicht Ton per Shared-Memory-Ring an einen Hilfsprozess `lz-streamer plugin-host`. Der lädt VST3 (MIT-SDK).
   - Plugin-Latenz und ein Block Zusatzlatenz gehen in die Latenzabfrage. Stirbt der Hilfsprozess, schaltet das Element sofort auf Bypass, und der Aufpasser startet ihn neu.
   - Zustand im Profil, Parameter in der Weboberfläche.
   - **Abnahme:**
     - LZ Defeedback (VST3) und ein LSP-Plugin laden.
     - Gewaltsames Beenden des Hilfsprozesses ergibt keine Tonlücke > 1 Block und keinen Kanalneustart.
     - A/V-Versatz bleibt.
     - Test unter macOS und Linux.
   - **Abh.:** keine (A1 hilfreich).

7. **P2 – CLAP über `clack-host` und AU am Mac (M)**
   - Derselbe Hilfsprozess lädt CLAP-Plugins (clack) und am Mac AUv2/AUv3 über AudioToolbox.
   - Plugin-Liste per Scan, Ergebnis zwischengespeichert, abstürzende Plugins beim Scan auf eine Sperrliste.
   - **Abnahme:** je ein CLAP- und ein AU-Plugin laufen mit denselben Tests wie P1; Scan überlebt ein absichtlich abstürzendes Test-Plugin.
   - **Abh.:** P1.

8. **U1 – Untertitel aus SDI durchreichen und im Decoder ausgeben (M)**
   - Der Encoder holt mit `decklinkvideosrc output-cc=true` die CC-Meta und schreibt sie mit `h264ccinserter`/`h265ccinserter` in die SEI. Der Decoder holt sie mit `h26xccextractor` und gibt sie mit `decklinkvideosink cc-line` ins VANC zurück.
   - Die Eingangsanzeige zeigt „CC 608/708 vorhanden“ (Logik wie `lz-scopes/src/vanc.ts`).
   - **Abnahme:**
     - Integrationstest mit `sccparse`-Datei als Quelle → SRT → Decoder → `cea608tott` vergleicht Text.
     - ffprobe meldet „Closed Captions“ im TS.
     - Test an echter DeckLink offen, als Punkt aufgeführt.
   - **Abh.:** Fahrplan Punkt 3 (dieses Issue ersetzt dessen CC-Teil).

9. **U2 – Live-Untertitel aus Spracherkennung (L, Pflicht)**
   - `transcriberbin` mit umschaltbarer Engine: lokal `whispertranscriber` mit ggml-Modell, Modell-Download in der Oberfläche; Cloud über Speechmatics/Deepgram/AWS mit eigenem Schlüssel, nie im Repo.
   - Sprache, Tonspur als Quelle (nach Matrix, vor Lautheit oder auf K1-Sprachsignal), Roll-up oder Pop-on, Wortersetzungsliste (Namen, Fachbegriffe).
   - Ausgabe als 608 (Kanal 1) und 708 (Dienst 1) in der SEI.
   - **Abnahme:**
     - Deutsche Testaufnahme ergibt eine WER-Messung (dokumentiert).
     - Textverzug Median ≤ 5 s mit Whisper „small“ oder „medium“ am M1 Pro.
     - YouTube-Test mit „Embedded 608/708“ zeigt die Untertitel.
     - Kanal läuft weiter, wenn die Engine abstürzt.
   - **Abh.:** U1 (gemeinsamer Inserter-Weg).

10. **U3 – Untertitel einbrennen per OGraf (M)**
    - lz-streamer bekommt einen Grafik-Bildeingang nach dem Renderer-Protokoll von lz-vision-mixer (RGBA je Ebene per TCP). Der Renderer wird als Hilfsprozess gestartet und lädt `lz-captions`.
    - Text kommt aus U2, aus OSC `/lzvm/caption` (Intercom) oder per `POST /api/caption`. Einbrennen ist je Ausgang wählbar, z. B. SRT sauber und RTMP mit Untertitel.
    - Rückfallebene ohne Renderer: `cea708overlay`.
    - **Abnahme:** Untertitel im Bild bei 1080p50 ohne Bildverlust; Renderer-Absturz lässt das Bild ohne Grafik weiterlaufen; Snapshot-Test des Vorschaubilds.
    - **Abh.:** U2 für die Spracherkennung, sonst unabhängig.

11. **U4 – OGraf-Grafiken allgemein: Liedtexte, Bauchbinde (S)**
    - Derselbe Grafikweg nimmt beliebige OGraf-v1-Grafiken (lz-presenter-Liedtexte, `lz-lower-third`).
    - Steuerung über API und Companion: laden, abspielen, weiter, aktualisieren, aus.
    - **Abnahme:** lz-presenter steuert die Liedtextgrafik im Streamer-Bild; Companion-Tasten funktionieren.
    - **Abh.:** U3.

12. **U5 – Untertiteldateien zur Aufnahme und WebVTT (S)**
    - Neben jeder Aufnahme entstehen `.srt` und `.vtt` (`cea608tott`/`jsontovtt`), optional `.scc`.
    - Bei HLS-Ausgabe eine WebVTT-Untertitel-Rendition, wenn `hlscmafsink` das trägt, sonst eigener Segmentierer.
    - **Abnahme:** ffprobe und ein Player (VLC) zeigen die Untertitel synchron zur Aufnahme, Abweichung < 200 ms.
    - **Abh.:** U2; HLS aus Fahrplan Punkt 5.

13. **U6 – Übersetzte Untertitel (M)**
    - `translationbin` oder Canary bzw. Cloud-Übersetzung erzeugt eine zweite Untertitelsprache (608 CC3 bzw. 708 Dienst 2, eigene VTT).
    - Hinweis in der Oberfläche: YouTube zeigt nur eine Spur.
    - **Abnahme:** Deutsch → Englisch live mit gemessenem Zusatzverzug; beide Spuren im TS.
    - **Abh.:** U2.

14. **U7 – DVB-Untertitel im TS (S, optional)**
    - Text aus U2 → `dvbsubenc` → `mpegtsmux`, mit Sprachkennung, für IPTV/Multicast-Empfänger (z. B. lz-media-station-Kanal).
    - **Abnahme:** VLC und ein Fernseher mit DVB-IPTV zeigen die Untertitel; TSDuck meldet die Subtitling-Descriptor korrekt.
    - **Abh.:** U2; Broadcast-TS aus Fahrplan Punkt 1 hilfreich.

15. **U8 – YouTube-Untertitel per HTTP POST (S, auf Nachfrage)**
    - Client für die signierte Ingest-URL, Zeilen mit Folgenummer und Zeitstempel, Herzschlag.
    - Nur sinnvoll, wenn 608 im Strom nicht geht (z. B. fremder Encoder). YouTube verlangt dafür 30 oder 60 s Verzögerung.
    - **Abnahme:** Test-Livestream zeigt die Untertitel.
    - **Abh.:** U2.

16. **I1 – Steuerung über Companion und lz-camera-bridge (S)**
    - API-Wege und Companion-Aktionen: Untertitel an/aus, Klare Sprache an/aus, Lautheitsprofil, Plugin-Bypass, Grafik ein/aus.
    - Eigenschaften so benennen, dass sie später als NMOS-IS-12-Eigenschaften der Bridge abbildbar sind.
    - **Abnahme:** Companion-Modul (privat) schaltet alles; Zustand erscheint in `/api/state`.
    - **Abh.:** jeweilige Funktion; Fahrplan Punkt 9.

17. **R1 – Anfrage Fraunhofer IAIS Live-Subtitling (S, keine Entwicklung)**
    - Bei Digital Media anfragen: API, Self-Hosting, Lizenzmodell und Preis.
    - Nur dann eine weitere Engine in `transcriberbin`, wenn ein Behörden- oder Landtagskunde ansteht.
    - **Abnahme:** Antwort dokumentiert in diesem Dokument.
    - **Abh.:** U2.

**Vorgeschlagene Reihenfolge:**
1. A1
2. U1 → U2 → U3 (Untertitel zuerst, Pflicht)
3. A3 → A2
4. P1
5. K1
6. U4, U5
7. P2, U6, I1
8. U7, K2, U8, R1
