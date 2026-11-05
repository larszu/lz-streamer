# Recherche: Hardware der Kompakt-Encoder, günstige Plattformen, Scaler-Verfahren

Stand: 05.11.2026. Ergänzt `vergleich-hardware-latenz.md` (dort §4 Hardware-Empfehlung, ≈ 2 600–2 950 €).

Legende: **[H]** Herstellerangabe, nicht geprüft · **[D]** Angabe Dritter (Händler, Presse, Forum) · **[I]** Indiz/Vermutung, nicht belegt · **[S]** eigene Schätzung/Rechnung · **[M]** lokal geprüft.
Preise sind Listenpreise vom Recherchetag, ohne Gewähr.

---

## 1. Kurzfassung

- **Was in den Kompaktgeräten steckt, ist fast nirgends öffentlich.** Belegt sind nur zwei Fälle: **AMD/Xilinx Zynq UltraScale+ MPSoC** in der Kiloview N60 (AMD-Fallstudie) und ein **Intel Core i7 der 6. Generation** in der Epiphan Pearl-2. Bei der Lynx PEC 1464 zeigt das Parametervokabular der Firmware ebenfalls auf die Zynq-VCU (eigene Analyse). Für alle anderen gibt es nur Indizien.
- **Das Muster:** Geräte mit **UHD 4:2:2 10-bit** (Teradek Prism Flex, Makito X4, Kiloview N50/N60, Matrox Monarch EDGE E10) haben eigenes Codec-Silizium: Zynq-VCU oder einen Codec-ASIC wie den Socionext MB86M31. Sie brauchen dafür 16–18 W. Geräte mit Consumer-SoC oder x86 bleiben bei 4:2:0 oder bei 1080p.
- **Für lz-streamer reicht deutlich weniger als die bisherige Empfehlung.** Der Engpass ist nicht die Rechenleistung, sondern zwei Dinge:
  1. 12G-Capture mit Linux-Treiber: DeckLink 8K Pro Mini ab ≈ 690 €, Magewell Eco Capture 12G M.2 ≈ 700 €.
  2. Ein Encoder, der **HEVC 4:2:2 10-bit** kann. Das können NVIDIA Blackwell, Intel Arc (Alchemist/Battlemage), Lunar Lake und Panther Lake sowie Apple-VideoToolbox. **Nicht** können es Meteor Lake und Arrow Lake (dort nur Decode) und nach Belegen auch RK3588 und Jetson nicht.
- **Bauvorschlag B (Ryzen/Core-ITX + RTX 5060 oder Arc B580 + DeckLink 8K Pro Mini) ≈ 1 500–1 750 €** ersetzt das 2 600–2 950-€-Gerät ohne Abstriche bei 1–2 Kanälen. Vorschlag A (N305 + M.2-Capture, ≈ 1 000–1 100 €) reicht für UHD 4:2:0. Siehe §3.
- **Teranex, greenMachine und Co. skalieren mit klassischer Signalverarbeitung, nicht mit KI:**
  - kantenrichtungsabhängige Filter („multi-directional diagonal filters“)
  - pixelweise bewegungsadaptives Deinterlacing (PixelMotion)
  - temporal-rekursive Rauschminderung
  - bei Normwandlern Phasenkorrelation (Snell Alchemist)

  Latenz laut Hersteller: Teranex AV „2 Frames“ bzw. „ab 67 ms“, greenMachine 1080p50→2160p50 **20 ms (1 Frame)**.
- **Teranex-Niveau für progressives HD→UHD** ist in Software erreichbar. Nötig ist ein kantenadaptiver 2×-Skalierer auf der GPU (RAVU-/FSRCNNX-Klasse) mit Antiringing und leichtem Nachschärfen. Das kostet unter 1 Frame Latenz und auf einer Mittelklasse-GPU wenige Prozent Last [S]. Teurer ist **1080i**: Dort braucht man ein bewegungsadaptives Deinterlacing (bwdif/w3fdif/yadif-Klasse, +1 Halbbild) vor dem Skalierer.

---

## 2. Was in den Kompakt-Encodern steckt

| Gerät | 12G-Eingang | Encode-Spitze | SoC / Codec | Leistung | Preis | Belege |
|---|---|---|---|---|---|---|
| **Lynx PEC 1464** | ja | HEVC/AVC UHD, 4:2:2 nur bis 1080p [H] | **Zynq UltraScale+ EV (VCU)** [I]: Parameter 1:1 wie VCU-ctrlsw (`GopMode lowDelayP`, `GDRmode`, `SubFrameLatency`); Status `statusBootedFPGA`, `statusTemperatureSOC` | 16 W lt. Handbuch, 8 W lt. Produktseite [H] | – | `~/Projekte/lynx-analyse/docs/pec-1464-analyse.md` §0/§1 |
| **Kiloview N60 / N50** | N50 ja (N60 HDMI) | UHD60, HEVC/AVC/NDI, **4:2:2 10-bit HDR** [H] | **AMD Zynq UltraScale+ MPSoC**: FPGA + Arm, „baseband video IP for SDI“ [H, AMD]. Für die N50 als Schwestermodell angenommen [I] | 16 W max [H] | – | [AMD-Fallstudie](https://www.amd.com/content/dam/amd/en/documents/resources/case-studies/kiloview-case-study.pdf), [Kiloview-Katalog 2026](https://enstatic.kiloview.com/wp-content/uploads/2026/07/PRODUCT-CATALOG.pdf) |
| **Kiloview E3** | nein (3G-SDI, HDMI 4K30) | HEVC/AVC | nicht öffentlich; Händler sprechen nur von „updated chipset“ [D] | < 6 W lt. Katalog [H, Zuordnung zur Katalogzeile ungeprüft] | – | [Videoguys](https://videoguys.com/products/kiloview-e3-dual-channel-4k-hdmi-3g-sdi-hevc-video-encoder) |
| **Kiloview P3** | nein | Bonding-Encoder | nicht öffentlich | – | – | [Handbuch](https://www.manualslib.com/manual/3678380/Kiloview-P3.html) |
| **Teradek Prism Flex Mk II** | ja (+ HDMI 2.0) | **4Kp60 4:2:2 10-bit**, HEVC/AVC, HDR PQ/HLG, 3D-LUT [H] | nicht öffentlich. Wegen 4:2:2 10-bit UHD60 bei 18 W wahrscheinlich VCU oder Codec-ASIC [I] | **18 W** [H] | – | [Datenblatt Mk II](https://cdn.shopify.com/s/files/1/0095/4332/files/Teradek_Prism_Flex_Mk_II_Technical_Specs_0324.pdf) |
| **Haivision Makito X4** | ja | 1 × UHD oder 4 × HD, „up to eight encoding cores“, 8/10 bit, 4:2:2 [H] | nicht öffentlich [I: Codec-Silizium wie oben] | **18 W pro Blade** [H] | ≈ 6 000 $ (1 Kanal, s. Vergleichsdoku) | [Produktübersicht](https://doc.haivision.com/MakitoX4Enc/1.8/product-overview), [Leistung](https://doc.haivision.com/MakitoX4Enc/1.5/dimensions-weight-power) |
| **Matrox Monarch EDGE** | E10: 1 × 12G + 3 × 3G | H.264, 10-bit-Variante mit 4:2:2 [H] | nicht öffentlich. Intel Quick Sync scheidet aus, weil es kein AVC 4:2:2 kodiert (§3.1) [I] | **45 W** (Mittel) [H] | 5 400–10 700 $ | [Tech Specs](https://video.matrox.com/en/products/encoders-decoders/monarch-edge-series/monarch-edge-tech-specs) |
| **Epiphan Pearl-2** | 2 × 12G | 4K-Lizenz, mehrere Kanäle | **Intel Core i7, 6. Generation** [H] → x86 mit Software/Quick Sync | – | – | [Epiphan-Forum](https://epiphan.com/forum/t/pearl-versus-pearl-2/620), [Händler](https://www.scan.co.uk/products/epiphan-pearl-2) |
| **Epiphan Pearl Nano** | 1 × 12G | H.264 bis **1080p60** [D] | nicht öffentlich (Doku-Seiten blocken automatische Abrufe) | Netzteil 12 V/2,5 A [D] | – | [Markertek](https://www.markertek.com/product/epip-pearl-nano/epiphan-esp1610-pearl-nano-all-in-one-video-production-system-12g-sdi-hdmi-2-0-streamer-and-recorder) |
| **Epiphan Pearl Nexus** | nein (3G) | 1080p | nicht öffentlich. Die Pearl-Familie ist x86 [I] | – | – | [Markertek](https://www.markertek.com/product/epip-pearl-nexus/epiphan-pearl-nexus-automated-video-capture-device-2x-hdmi-1x-3g-sdi-2x-usb-inputs-1080p) |
| **AJA HELO Plus** | nein (3G) | H.264 1080p60 | nicht öffentlich | 60 W (Netzteilangabe) [D] | 1 800–2 100 € | [Handbuch v2.1](https://www.aja.com/support/item-download/10154/AJA_HELO-Plus_Manual_v2.1r3.pdf) |
| **Magewell Ultra Encode** | nein (SDI Plus: 6G, 4K30) | HEVC/AVC | nicht öffentlich | – | – | [TV Technology](https://www.tvtechnology.com/news/magewell-unveils-pair-of-new-ultra-encode-family-live-media-encoders) |
| **Mekotronics R58-4x4-3U** (Referenz) | 1 × 12G + 3 × 3G | H.265/H.264 bis 8K30 [D] | **Rockchip RK3588**, SDI über vier SDI→MIPI-CSI-Wandler [D] | – | 735 $ (ab 10 Stück) [D] | [CNX Software](https://www.cnx-software.com/2025/03/05/rockchip-rk3588-4k-video-encoder-features-four-sdi-inputs-four-sdi-loop-outputs/) |

**Weitere Belege zum Codec-Silizium:**
- Die Zynq UltraScale+ EV hat einen fest verdrahteten H.264/H.265-Block (VCU) für bis 4K60. Der kann 4:2:2 10-bit [H].
- AMD nennt weitere Kunden: XVTEC XVC-Ultra mit „< 40 ms“ Ende-zu-Ende und Talon, lüfterlos [H]. Quelle: [Nasdaq/Xilinx-Pressemitteilung](https://www.nasdaq.com/press-release/xilinx-and-leading-broadcast-and-av-system-and-ip-integrators-deliver-complete).
- Socionext MB86M31 ist ein HEVC-Encoder-IC mit 4:2:2 10-bit [H]. Er steckt bei Adtec, Advantech-Karten und Fujitsu [H]. Quelle: [Socionext/Adtec](https://socionextus.com/pressreleases/compact-4k-end-to-end-hevc-solution-for-optimized-video-processing/).
- Eine Zuordnung zu Teradek, Haivision oder Matrox ist **nicht belegt**.

**Folgerung aus der Tabelle:**
- 12G-Eingang allein ist billig. Das zeigt die RK3588-Box für 735 $ und die Pearl Nano, die nur 1080p kodiert.
- Teuer ist **UHD 4:2:2 10-bit mit niedriger Latenz**. Das liefert bei den Kompaktgeräten immer ein FPGA-SoC mit VCU oder ein Codec-ASIC.
- Ein PC bekommt dasselbe heute über NVENC (Blackwell), Intel VDEnc (Arc/Lunar Lake/Panther Lake) oder VideoToolbox. Er braucht dafür mehr Leistung (≈ 40–120 W statt 18 W [S]).

---

## 3. Folgerung für lz-streamer

### 3.1 Wer kann HEVC 4:2:2 10-bit kodieren?

| Plattform | HEVC 10-bit 4:2:0 | **HEVC 10-bit 4:2:2** | AVC 4:2:2 | AV1 | Beleg |
|---|---|---|---|---|---|
| Intel Tiger/Alder/Raptor Lake, **N100/N305 (ADL-N)** | E | nur **Es** (PAK + Shader, nur im Full-Feature-Build des Treibers) | nein | nein | [intel/media-driver README](https://github.com/intel/media-driver/blob/master/README.md) [H] |
| Intel **Meteor Lake / Arrow Lake** (Core Ultra 100/200) | E | **nur Decode** | nein | E | dito |
| Intel **Arc Alchemist (DG2)**, **Battlemage (BMG)**, **Lunar Lake**, **Panther Lake** | E | **E (VDEnc)** | nein | E | dito |
| NVIDIA Blackwell (RTX 50) | ja | ja | ja | ja | `nachbau-konzept.md` §3 [H, dort nicht neu geprüft] |
| Apple M-Serie (VideoToolbox) | ja | Profil `HEVC_Main42210` vorhanden; OBS bietet „main 4:2:2 10“ ab macOS 12.3; FFmpeg hatte es 2024 noch nicht [D] | – | M4: nein [I] | [FFmpeg-Ticket #11097](https://fftrac-bg.ffmpeg.org/ticket/11097), [OBS-Commit](https://git.tjdev.de/mirror/obs-studio/commit/e461ec4be1) |
| Rockchip RK3588 (MPP) | 8-bit belegt, 10-bit nicht belegt | `hevc_rkmpp` nimmt 4:2:2-Eingang an; ob kodiert oder gewandelt wird, ist offen [D] | ? | nein | [Jellyfin](https://jellyfin.org/docs/general/post-install/transcoding/hardware-acceleration/rockchip/), [ffmpeg-rockchip](https://github.com/nyanmisaka/ffmpeg-rockchip/wiki/Encoder) |
| Jetson Orin NX/AGX | ja, max. 4K [D] | nicht belegt | nicht belegt | nein | [NVIDIA-Forum](https://forums.developer.nvidia.com/t/jetson-hardware-accelerated-encoders-max-performance/313732); Orin Nano ohne NVENC (Konzept §3) |

Lesart:
- **4:2:2 10-bit gibt es bei Intel nur als HEVC.** AVC 4:2:2 kann kein Intel-Chip. Wer AVC-4:2:2-IRDs beliefern muss, braucht NVIDIA Blackwell.
- **Ein „Core Ultra“-Mini-PC ist eine Falle.** Meteor Lake und Arrow Lake kodieren kein 4:2:2. Lunar Lake und Panther Lake können es.
- Für **Streaming zu Plattformen** (YouTube, CDN) reicht 4:2:0 10-bit. 4:2:2 braucht man nur für Contribution an Sender oder Postproduktion.

### 3.2 Bezahlbare 12G-Capture mit Linux

| Karte | Bauform | Kanäle | Linux | Preis | Beleg |
|---|---|---|---|---|---|
| **DeckLink 8K Pro Mini** | PCIe 3.0 x8, Low-Profile | 4 × 12G bidirektional (Single/Dual/Quad-Link) | Desktop Video, GStreamer `decklinkvideosrc` | **789 $**, EU ≈ 690–745 € | [Blackmagic-Modelle](https://www.blackmagicdesign.com/products/decklink/models), [Full Compass](https://www.fullcompass.com/prod/634405-blackmagic-design-decklink-8k-pro-mini-low-profile-capture-playback-card) |
| DeckLink 8K Pro G2 | PCIe, volle Höhe | 4 × 12G + Referenz | dito | 855 $ | [Blackmagic](https://www.blackmagicdesign.com/products/decklink/models) |
| DeckLink 4K Extreme 12G | PCIe 2.0 x8 | 1 × 12G in/out | dito | 1 245 $ | dito |
| **Magewell Eco Capture 12G SDI 4K Plus M.2** | **M.2 2280**, PCIe 3.0 x4 | 1 × 12G, 4K60 | V4L2-Treiber [H] | ≈ 580–700 € | [Magewell](https://magewell.com/products/eco-capture-12g-sdi-4k-plus-m2), [Datenblatt](https://cn.magewell.com/static/tech-specs/Eco_Capture/EcoCaptureSDI_4K_Plus_M2.pdf), [avgear](https://avgear.shop/en/magewell-eco-capture-12g-sdi-4k-plus-m2-11561-video-capture-card-pcie-gen3-x4-557) |
| Magewell Pro Capture SDI 4K Plus | PCIe 2.0 x4 | 1 × 12G (oder 2 × 6G/4 × 3G) | V4L2, x86 und ARM [D] | ≈ 950 € | [avgear](https://avgear.shop/en/magewell-pro-capture-sdi-4k-plus-11180-video-capture-card-pcie-gen2-x4-44) |
| UltraStudio 4K Mini | Thunderbolt 3 | 1 × 12G + HDMI | Desktop Video | ≈ 1 000 € (aus Vergleichsdoku, ungeprüft) | – |
| AJA, Deltacast, Yuan 12G | PCIe | – | – | in dieser Runde keine Preise gefunden | – |

Zur Einordnung: Die Vergleichsdoku rechnete für die DeckLink 8K Pro mit ≈ 1 300 €. Laut Blackmagic-Preisliste kosten 8K Pro G2 und 8K Pro Mini heute 790–855 $. **Allein das spart ≈ 500 €.** Ob die Mini-Variante einen Referenzeingang hat, ist offen. Für die Decoder-Rolle mit Genlock vor dem Kauf prüfen.

Kein Mini-PC mit N100/N305 hat einen PCIe-x8-Steckplatz. Dort passt nur die Magewell-M.2-Karte. Ob eine DeckLink über einen M.2→PCIe-Adapter mit 4 Lanes läuft, ist nicht belegt [I]. Rechnerisch reicht die Bandbreite: UHD50 4:2:2 10-bit ≈ 8,3 Gbit/s, PCIe 3.0 x4 ≈ 31 Gbit/s [S].

### 3.3 Bauvorschläge

**A · Klein und billig: UHD 4:2:0, ein Kanal**

| Teil | Wahl | ca. € |
|---|---|---|
| Rechner | Mini-PC Intel N305 (8 Kerne), 16 GB, freier M.2-2280-Schlitz für die Capture-Karte | 300–400 [S] |
| Capture | Magewell Eco Capture 12G SDI 4K Plus M.2 | 600–700 |
| **Summe** | | **≈ 1 000–1 100** |

- **Kann:**
  - 1 × UHD50 HEVC 10-bit 4:2:0 über Quick Sync. Ob UHD50 auf ADL-N in Echtzeit läuft, ist zu messen [I].
  - SRT, Aufnahme.
  - HD→UHD auf der iGPU über `vapostproc` (Hardware-Skalierer, Modus „hq“). Welches Filter Intel dort nutzt, ist nicht dokumentiert.
- **Grenzen:**
  - kein 4:2:2-Encode in brauchbarer Form (nur Shader-Pfad)
  - kein AV1
  - iGPU mit 24 EU, zu schwach für ML-Skalierung [S]
  - kein Referenzeingang
  - kein Bildausgang für die Decoder-Rolle

**B · Hauptgerät, ersetzt den 2 600–2 950-€-Vorschlag**

| Teil | Wahl | ca. € |
|---|---|---|
| Rechner | ITX- oder Kompakt-PC, Ryzen 5/7 oder Core i5, 32 GB, ein PCIe-x16- und ein x4/x8-Steckplatz | 600–750 [S] |
| GPU | **RTX 5060 / 5060 Ti** (Blackwell, NVENC 4:2:2, CUDA) oder **Arc B580** (VDEnc 4:2:2 HEVC, AV1, XMX) | 300–450 [S] |
| Capture | **DeckLink 8K Pro Mini** (4 × 12G) | 690–745 |
| **Summe** | | **≈ 1 600–1 950**; mit Arc und schlankem Gehäuse ≈ 1 500 |

- **Kann:**
  - 1 × UHD50 4:2:2 10-bit oder 4 × HD
  - Hochskalieren auf der GPU, auch ML-Verfahren (§4.5)
  - NVIDIA zusätzlich: AVC 4:2:2
- **Grenzen:**
  - 80–150 W statt 18 W [S]
  - Lüfter
  - Referenzeingang der Mini-Karte prüfen
- **Wahl RTX oder Arc:**
  - RTX: GStreamer `nvcodec` (`nvh265enc`, `cudaconvertscale`) ist ausgereift.
  - Arc: Quick Sync über `qsv`/`va` und AV1 zum selben Preis. Der Linux-Treiber für Battlemage braucht einen aktuellen Kernel (`xe`).

**C · Mobil und leise (Mac)**

| Teil | Wahl | ca. € |
|---|---|---|
| Rechner | Mac mini M4 (Basis) oder M4 Pro | 700 / 1 600 [S] |
| Capture | UltraStudio 4K Mini (Thunderbolt, 12G) | ≈ 1 000 (prüfen) |
| **Summe** | | **≈ 1 700–2 600** |

- **Kann:** HEVC 4:2:2 10-bit über VideoToolbox. Das Profil existiert, der GStreamer-Weg `vtenc_h265` mit 4:2:2 ist noch zu prüfen [I].
- **Grenzen:**
  - Am M1 Pro lag UHD50 HEVC an der Grenze [M, Vergleichsdoku]. Für die Basis-M4 ist das unbelegt.
  - Hochskalieren mit Metal-Shadern müsste neu gebaut werden.

**Nicht empfohlen:**
- **RK3588:** Die fertige 735-$-Box zeigt, dass es geht. MPP kann aber 4:2:2 und 10-bit nicht belegt kodieren, und GStreamer-Pflege kostet Aufwand.
- **Jetson Orin:** NVENC max. 4K, 4:2:2 nicht belegt, SDI-Frontend selbst bauen.
- **Eigenes SDI-Frontend:** Semtech GS12170 ist ein SDI/HDMI-Brückenchip, [Semtech](https://www.semtech.com/products/broadcast-video/av-interface-protocol-conversion/gs12170). Ein eigenes Frontend lohnt erst bei Stückzahl.

---

## 4. Wie die teuren Scaler arbeiten

### 4.1 Was die Hersteller sagen

| Gerät | Verfahren (Herstellerwortlaut, sinngemäß) | Latenz | Leistung / Preis | Beleg |
|---|---|---|---|---|
| **Blackmagic Teranex** (AV, Express) | „optical quality scaling“; „sub-pixel processing and **multi-directional diagonal filters**“ bei SD/HD/UHD; **PixelMotion**-Deinterlacing, patentiert, Apertur „pixel by pixel“; Bildratenwandlung mit Zwischenpixel-Berechnung; Pulldown-Erkennung; „**temporal recursive noise reduction**“; Szenenschnitt-Erkennung [H] | AV: „**2 frame latency**, available for select format conversions“; „latency as low as **67 ms**“ [H]. Express: keine Zahl | AV 42 W, 1 909 $; Express 1 575 $ [H] | [Conversions](https://www.blackmagicdesign.com/products/teranex/conversions), [Tech Specs AV](https://www.blackmagicdesign.com/nz/products/teranex/techspecs), [Modelle/Preise](https://www.blackmagicdesign.com/products/teranex) |
| Teranex 2D (Vorgänger, Test 2013) | Bildratenwandlung **linear interpoliert** → Geisterbilder bei schneller Bewegung [D] | – | – | [Definition Magazine](https://definitionmagazine.com/reviews/review-teranex-2d-standards-converter/) |
| **Lynx greenMachine titan** | Up/Down/Cross als Lizenz (UPXD), immer im Frame-Sync-Modus; Verfahren nicht veröffentlicht | **1080p50 → 2160p50: 20 ms**; 1080i50 → 2160p: 40 ms; 1080p25 → 1080p50: 60 ms; asynchrone Referenz + 1 Frame (max. 2) [H] | – | [Lynx Support](https://support.lynx-technik.com/support/solutions/articles/1000335530-greenmachine-processing-delay-frame-sync-mode-) |
| **AJA FS-HDR / FS4** | Colorfront Engine (HDR/WCG), Up/Down/Cross; Skalierverfahren nicht veröffentlicht | Standardlatenz in der gefundenen Doku nicht beziffert; einstellbare Zusatzverzögerung („Frame Delay“) [H] | – | [FS-HDR-Handbuch](https://www.manualslib.com/manual/1371964/Aja-Fs-Hdr.html?page=13) |
| **Snell/GV Alchemist Ph.C** | **Phasenkorrelation** (Bewegungsschätzung im Frequenzbereich, deshalb robust gegen Rauschen und Helligkeitssprünge), CleanCut, H/V-Apertur/Alias-Unterdrückung, H/V-Enhancement, Rauschminderung [H] | nicht im Datenblatt | **500 W, 3 HE, 20 kg** [H] | [Alchemist Ph.C-HD](https://www.thameside.tv/documents/Alchemist_PhC-HD.pdf) |
| Alchemist X (GV AMPP, Cloud) | Modi „High/Low Ph.C Motion Compensated Interpolation“, „Linear“, „Synchronous“ [H] | – | – | [Grass Valley](https://grassvalley.com/products/ampp-saas/alchemist-x) |
| Phasenkorrelation allgemein | FFT zweier Halbbilder → Phasendifferenz → inverse FFT → Korrelationsfläche mit Spitzen je Bewegung; liefert nebenbei ein Zuverlässigkeitsmaß | – | – | [TV Technology](https://www.tvtechnology.com/miscellaneous/standards-conversion), Patent [US5257102](https://patents.google.com/patent/US5257102) |
| **FOR-A FRC-9100** | bewegungskompensierte Wandlung, Up-Conversion; End-of-Sale [H] | nur einstellbare Verzögerung bis 8 Frames angegeben | – | [FOR-A](https://www.for-a.com/products/frc9100) |
| EVS GSU/HSU100/110 | Up-Converter | „as low as **1 field**“ bei kontrollierter Taktung [H] | – | [EVS-Datenblatt](https://evs.com/sites/default/files/2021-08/GSU-HSU100-110_PD.pdf) |
| Evertz, Ross, Snell KudosPro | – | in dieser Runde nichts Belastbares gefunden | – | – |

### 4.2 Was daraus über die Verfahren folgt

**Skalierer (räumlich)**
- „Multi-directional diagonal filters“ heißt **kantenrichtungsabhängige Interpolation**. Das Filter folgt der lokalen Kantenrichtung, statt nur waagrecht und senkrecht zu filtern. Das nimmt die Treppen an Schrägen weg, die ein separabler Lanczos stehen lässt [I, Deutung des Herstellertexts].
- Polyphasen-Filter mit vielen Taps (8–12 je Richtung) sind in Broadcast-Hardware Standard. Für Teranex ist die Tap-Zahl **nicht veröffentlicht**.
- „Apertur“ und „Enhancement“ (Alchemist) stehen für einstellbare Nachschärfung bzw. Detailanhebung nach dem Skalieren.

**Deinterlacing**
- **Bewegungsadaptiv:** Ruhige Pixel werden aus beiden Halbbildern gewebt, bewegte innerhalb des Halbbilds interpoliert (PixelMotion: „pixel by pixel“).
- **Bewegungskompensiert:** Bewegte Pixel werden entlang des Bewegungsvektors aus Nachbarhalbbildern geholt (Alchemist).
- Beide brauchen mindestens ein Halbbild Vorschau.

**Rauschminderung**
- „Temporal recursive“ mittelt mit dem bereits gefilterten Vorbild und hängt das an die Bewegungserkennung.
- Sie braucht keine Zukunftsbilder, also keine Zusatzlatenz.

**Normwandlung (Bildrate)**
- Hier unterscheiden sich die Klassen am stärksten:
  - linear, mit Geisterbildern (Teranex 2D laut Test)
  - bewegungsadaptiv
  - Phasenkorrelation (Alchemist)
- Für lz-streamer ist das nur bei 50 ↔ 59,94 relevant, nicht beim HD→UHD-Skalieren.

**Latenz-Einordnung**

| Klasse | typische Latenz | Grund |
|---|---|---|
| Rein räumlich, progressiv (1080p50 → 2160p50) | **≤ 1 Frame** (greenMachine 20 ms) | nur einige Zeilen Vorschau nötig, Rest ist Frame-Sync |
| Mit Deinterlacing (1080i → 2160p) | ≈ 2 Frames (greenMachine 40 ms) | Halbbild-Vorschau + Frame-Sync |
| Mit Bildratenwandlung | 2–3 Frames (greenMachine 60 ms; Teranex AV „ab 67 ms“) | Zukunftsbilder für die Interpolation |
| Asynchrone Referenz | + 1 Frame | Ausrichtung am Haustakt |

### 4.3 Wie gut sind sie in Tests?

- **Unabhängige Messungen (PSNR/SSIM/VMAF) für Teranex, FS-HDR oder greenMachine wurden nicht gefunden.**
- Es gibt nur Herstellertexte, Händlerkommentare und einen alten Test des Teranex 2D, der die lineare Bildratenwandlung kritisiert.
- Eine belastbare Aussage „lz-streamer erreicht Teranex-Qualität“ geht deshalb nur mit eigener Messung am geliehenen Gerät (§4.6).

### 4.4 Öffentliche Zahlen zu Software-Skalierern

Messung von artoriuz: 2× Luma, ein Testbild (vom Autor selbst „very unscientific“ genannt), Quelle [artoriuz.github.io](https://artoriuz.github.io/blog/mpv_upscaling.html) [D]:

| Verfahren | PSNR dB | SSIM | Abstand zu Lanczos |
|---|---|---|---|
| bilinear | 34,16 | 0,9735 | −2,4 dB |
| lanczos | **36,51** | 0,9799 | 0 |
| polar_lanczossharp (EWA) | 36,07 | 0,9786 | −0,4 dB |
| ravu-lite-ar-r3 (kantenadaptiv, trainiert) | 39,41 | 0,9873 | **+2,9 dB** |
| ravu-zoom-ar-r3 | 39,39 | 0,9880 | +2,9 dB |
| ArtCNN C4F16 (kleines CNN) | 42,68 | 0,9916 | +6,2 dB |
| ArtCNN C4F32 | 43,47 | 0,9923 | **+7,0 dB** |

- FSRCNNX steht in der älteren Fassung derselben Reihe vor Anime4K und RAVU [D].
- Läuft laut Nutzerberichten auf einer RX 470 in Echtzeit. Gemeint ist Wiedergabe, kein UHD50-Livebetrieb [D].
- Größere FSRCNNX-Varianten als `8-0-4-1` bringen laut Nutzern kaum Gewinn [D].

**NVIDIA Maxine Video Effects SDK, Linux** (Herstellerangabe, Quelle [docs.nvidia.com](https://docs.nvidia.com/maxine/vfx/latest/LinuxVFXSDK/PerformanceReference.html)):

| Effekt | Ein- → Ausgang | L40 | B40 |
|---|---|---|---|
| Video Super Resolution, Medium | 720p → 1440p | 0,81 ms | 0,58 ms |
| Video Super Resolution, Ultra | 720p → 1440p | 3,14 ms | 1,60 ms |
| Upscale (klassisch) | 720p → 1080p | 0,08 ms | 0,05 ms |

- Die Windows-Doku nennt für VSR 720p → 1080p: RTX 3080 3,3 ms, RTX 4090 1,4 ms, RTX 5090 1,1 ms [H].
- 1080p → 2160p hat 2,25 × die Ausgangspixel von 720p → 1440p. Hochgerechnet auf eine L40-Klasse-GPU sind das ≈ 2–7 ms je Bild, auf einer RTX 5060 vermutlich 2–3 × mehr [S].
- Das RTX Video SDK (VSR wie im Browser) nennt keine Zeiten: [developer.nvidia.com/rtx-video-sdk](https://developer.nvidia.com/rtx-video-sdk).

**Intel VPL**
- „AI-based super resolution“ ist seit VPL 2.13 vorhanden, laut Intel-Mitarbeiter noch experimentell, nur NV12 (8-bit 4:2:0) [D].
- Auf einer Arc A750 gab es bei 3840×2160 einen GPU-Hänger [D].
- **Für 10-bit-Broadcast unbrauchbar.** Quellen: [Phoronix](https://www.phoronix.com/news/Intel-oneVPL-2.13-Released), [libvpl #156](https://github.forks.workers.dev/intel/libvpl/issues/156).

**Real-ESRGAN, Topaz**
- Für Echtzeit-UHD50 nicht belegt.
- Real-ESRGAN-Ableger (TensorRT, mpv) zielen auf 1080p → 4K bei 24/30p Wiedergabe. Das „Quality“-Profil braucht eine RTX 4090 [D], [mpv-upscale-2x_animejanai](https://github.com/the-database/mpv-upscale-2x_animejanai).
- Dazu kommt: GAN-Verfahren **erfinden Details**. Das ist für Broadcast-Contribution unerwünscht, weil es Gesichter, Schrift und Texturen verändert [I].

### 4.5 Was lz-streamer für mindestens Teranex-Niveau braucht

**Heute** [M, lokal geprüft]:
- `src/pipeline.rs::scaler()` nutzt `videoscale method=lanczos` mit Standard-`envelope=2`, also kurzes Fenster, ohne Schärfung, im Gamma-Raum.
- GStreamer 1.28 hätte `envelope` bis 5, `sharpen` und `sharpness` sowie die Methoden spline/catrom/mitchell.
- Deinterlacing ist vorhanden: `deinterlace` mit `yadif`, `greedyh`, `tomsmocomp`.
- FFmpeg lokal: `bwdif`, `w3fdif` (BBC Martin Weston, 3 Halbbilder), `estdif`, `nnedi`, `hqdn3d`, `nlmeans`.

**Zielkette für HD → UHD:**

```
Eingang ─► [nur bei 1080i] bewegungsadaptives Deinterlacing (bwdif/yadif/w3fdif-Klasse, auf der GPU)
        ─► [optional] temporal-rekursive Rauschminderung (keine Zukunftsbilder)
        ─► 2×-Skalierer kantenadaptiv: Luma RAVU/FSRCNNX-Klasse, Chroma separabel (Lanczos/Polyphase)
        ─► Antiringing-Begrenzung (lokales Min/Max wie libplacebo)
        ─► leichte Nachschärfung / Detail (abschaltbar)
        ─► 10-bit-Pfad ohne Zwischenwandlung auf 8 bit
```

| Stufe | Verfahren | Latenz | Rechenlast 2160p50 [S] |
|---|---|---|---|
| Deinterlacing 1080i50 | bwdif/yadif (1 Halbbild Vorschau) | **+20 ms** | CPU 1–2 Kerne oder GPU < 1 ms/Bild |
| Rauschminderung | temporal rekursiv | +0 (nur Rechenzeit) | GPU < 1 ms |
| Skalierer, Basis | Lanczos3 (6 Taps je Richtung) | Rechenzeit, keine Bildvorschau | ≈ 415 Mpx/s × 12 MAC ≈ 5 GMAC/s Luma, CPU machbar (8 Threads am M1 Pro [M]) |
| Skalierer, Ziel | RAVU-lite r3 oder FSRCNNX 8-0-4-1 | dito, ≈ 1–3 ms GPU | FSRCNNX ≈ 2,8 kMAC je Eingangspixel × 104 Mpx/s ≈ 0,3 TMAC/s ≈ 0,6 TFLOPS: unter 5 % einer RTX 5060, zu viel für eine N100-iGPU (≈ 0,3 TFLOPS) |
| Skalierer, Oberklasse | ArtCNN C4F16/F32 oder Maxine VSR | dito, ≈ 3–10 ms GPU | ≈ 3 TMAC/s ≈ 6 TFLOPS FP32, mit FP16/Tensorkernen deutlich weniger; Mittelklasse-GPU nötig |
| Ausrichtung | Ausgabe an Bildtakt | + 0–1 Frame | – |

**Latenzbudget:** Progressiv ist ≤ 1 Frame (20 ms) wie die greenMachine erreichbar. Mit 1080i sind es ≈ 1,5–2 Frames. Damit liegt lz-streamer auf oder unter Teranex AV (2 Frames / 67 ms).

**Einbau in GStreamer**
- GStreamer hat keinen fertigen kantenadaptiven GPU-Skalierer [I].
- **Weg 1:** eigener Shader als `glshader` oder in Vulkan bzw. CUDA. Vorlage RAVU-lite, ein einziger Durchlauf mit trainierten Gewichten als Tabelle.
- **Weg 2:** libplacebo als Rust-Bindung mit mpv-Shadern (FSRCNNX/RAVU/ArtCNN). Das bringt Antiringing und Linear-Light-Optionen gleich mit.
- **Weg 3, nur NVIDIA:** Maxine VFX SDK über ein eigenes Element. Dagegen sprechen Lizenz und SDK-Bindung.
- **Ohne GPU-Shader** für einen schnellen Gewinn: `envelope=3` (Lanczos3), maßvoll `sharpen`, Chroma getrennt behandeln. Ein Gewinn gegenüber heute ist zu messen. Kantenadaptiv wird es dadurch nicht.

### 4.6 Zielwerte für die Messung (Vorschlag)

Es gibt keine veröffentlichten Teranex-Messwerte. Diese Ziele sind deshalb aus den öffentlichen Software-Zahlen (§4.4) abgeleitet [S]:

| Messgröße (UHD-Original → HD mit bekanntem Kernel → UHD, gegen Original) | Mindestziel | Gutes Ziel |
|---|---|---|
| PSNR-Y gegenüber heutigem `lanczos`, envelope 2 | ≥ +1,5 dB | ≥ +3 dB (RAVU-Niveau) |
| SSIM-Y | ≥ +0,005 | ≥ +0,008 |
| VMAF (UHD-Modell) | ≥ +3 Punkte | ≥ +6 Punkte |
| Treppen an Schrägen (Zonenplatte, Schrifttest) | sichtbar weniger als Lanczos | keine sichtbaren Treppen bei 2× |
| Ringing (Kantentest) | nicht stärker als Lanczos | sichtbar schwächer (Antiringing) |
| Latenz Skalierstufe | < 1 Frame | < 5 ms |
| Echtzeit | 2160p50 durchgehend, keine verworfenen Bilder über 1 h | dazu zweiter Kanal |

- Die Dezibel-Abstände stammen aus einem Einzelbild-Test (artoriuz). Der Kollege sollte sie auf eigenen Broadcast-Testbildern neu kalibrieren: Zonenplatte, Schrift, Gesichter, Rauschen.
- **Ein echter Vergleich braucht das Gerät.** Eine Teranex Express (1 575 $) oder greenMachine leihen und dieselbe Testfolge per DeckLink durchschicken. Die Rückaufnahme in lz-scopes wird gegen dasselbe Original gemessen.

---

## 5. Offene Punkte

- Chips in Teradek, Makito X4, Matrox, Epiphan Nano/Nexus, Magewell und AJA HELO sind nicht öffentlich. Der nächste Weg wären FCC-Innenfotos über die FCC-ID am Gerät, GPL-Quellpakete der Hersteller oder Kernel-Strings aus Firmware-Images, wie bei der Lynx.
- Den Referenzeingang der DeckLink 8K Pro Mini klären.
- An echter Hardware messen:
  - Intel ADL-N: UHD50 HEVC-Encode in Echtzeit
  - Intel `Es`-Pfad für 4:2:2
  - RK3588: 10-bit- und 4:2:2-Encode
  - Basis-M4: UHD50 mit `vtenc_h265` und 4:2:2
- Latenzzahlen für Evertz, Ross und FS-HDR sind nicht gefunden.
