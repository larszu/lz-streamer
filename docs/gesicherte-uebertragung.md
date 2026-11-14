# Gesicherte Übertragung über eine Empfangsstelle

Funktionen nach dem Vorbild von Resi (RSP, Resi Studio, Multisite-Decoder), mit eigener Technik umgesetzt.

## Ablauf

```
Encoder ──TS-Segmente (HTTP PUT, Prüfsumme, Quittung)──▶ Empfangsstelle ──▶ Ziele (YouTube, Facebook, Twitch, Vimeo, RTMP, SRT, UDP)
   │  Spool auf Platte                                    │  Ablage je Sendung      ──▶ Web-Player /player (Live + Mediathek)
   │  (überlebt Netz- und Kanalausfall)                   │  feste Verzögerung       ──▶ Standort-Decoder (HTTP-Abruf, bildgleich)
```

- **Encoder:** Ein eigener Segmentierer schreibt TS-Segmente (Vorgabe 2 s, alle Tonspuren, Untertitel in der SEI) in den Spool. Der Uploader läuft im Aufpasser-Prozess, mehrere Uploads parallel (Vorgabe 3). Ein Segment wird erst nach der Quittung gelöscht. Ohne Netz wächst der Spool (Vorgabe höchstens 30 min) und wird danach in Reihenfolge nachgeschickt.
- **Empfangsstelle** (Rolle „Empfangsstelle“, gleiches Programm, z. B. auf einem Server mit `--bind 0.0.0.0`): prüft die Prüfsumme (BLAKE2s), legt die Segmente je Sendung ab und spielt sie zur Aufnahmezeit plus Verzögerung aus (Vorgabe 90 s). Ist ein Ausfall kürzer als die Verzögerung, sieht niemand eine Lücke. Bei längeren Ausfällen geht es dort weiter, wo es abriss, und die Verzögerung wächst um die Ausfallzeit. „Auf Soll-Verzögerung zurückspringen“ holt auf.
- **Ziele:** je Ziel eine eigene kleine Pipeline. Ein abgelehntes Ziel stört die anderen nicht und verbindet nach 5 s neu, ab dem nächsten Segmentanfang (Keyframe). Je Ziel ist eine Zusatzverzögerung möglich. RTMP wird ohne Neukodierung aus dem TS umgepackt (H.264 + erste Tonspur).
- **Web-Player** (`/player`, öffentlich, einbettbar mit `?embed`): Live mit Ton- und Untertitelwahl (hls.js, Safari nativ), Knopf unter dem Bild (z. B. Spenden), Mediathek vergangener Sendungen mit Kapiteln aus den Marken. Noch nicht ausgestrahlte Segmente liefert die Empfangsstelle öffentlich nicht aus.
- **Standort-Decoder** (Decoder, Quelle „Empfangsstelle“): holt die Segmente in einen lokalen Cache und gibt sie zur Aufnahmezeit plus eingestellter Verzögerung aus. Standorte mit derselben Verzögerung laufen bildgleich, wenn die Rechneruhren per NTP gehen. Eine bestimmte Sendung mit großer Verzögerung ergibt eine zeitversetzte Wiedergabe.
- **Kopplung statt Stream-Schlüssel:** Die Empfangsstelle erzeugt einen sechsstelligen Code (10 min gültig, einmalig). Encoder bzw. Standort-Decoder tragen Adresse und Code ein und bekommen einen Gerätezugang. Die Empfangsstelle speichert nur dessen Hash; entkoppeln geht in der Geräteliste.
- **Marken:** „Marke setzen“ am Encoder (`POST /api/rsp/mark`) wird mit den Segmenten übertragen und erscheint im Player als Kapitel.
- **Zuschauer:** Der Player meldet sich alle 20 s. Je Sendung: gleichzeitig, höchstens gleichzeitig, verschiedene Zuschauer, gesehene Minuten.

## API

| Weg | Wer | Zweck |
|---|---|---|
| `POST /api/rsp/pairing` | Verwalten (Empfangsstelle) | Kopplungscode erzeugen |
| `POST /api/rsp/pair` | Gerät mit Code | Gerätezugang holen |
| `PUT /api/rsp/segment` | Gerät | Segment abliefern (`X-Name`, `X-Wall-Ms`, `X-Dur-Ms`, `X-Hash`) |
| `GET /api/rsp/index`, `/api/rsp/seg/{sendung}/{segment}` | Gerät | Abruf durch Standort-Decoder |
| `GET /api/rsp/sessions`, `DELETE …/{id}`, `GET …/{id}/download` | Ansehen/Verwalten | Sendungen, Zuschauer, als eine TS-Datei laden |
| `GET /api/rsp/devices`, `DELETE …/{id}` | Verwalten | Geräte |
| `POST /api/rsp/catchup` | Bedienen | auf Soll-Verzögerung springen |
| `POST /api/rsp/pair-with`, `/api/rsp/unpair`, `/api/rsp/mark` | Encoder/Standort | koppeln, lösen, Marke |
| `/player`, `/live/index.m3u8`, `/vod/{id}/index.m3u8`, `/api/player/*` | öffentlich | Player, Live, Mediathek, Zuschauerzählung |

## Grenzen

- Facebook und YouTube werden über RTMP(S) mit Stream-Schlüssel angebunden, nicht per Konto-Anmeldung (OAuth).
- RTMP-Ziele brauchen H.264.
- Zuschauerzählung im Speicher, je Sendung als Datei gesichert; keine Geo- oder Geräteauswertung.
- Die Empfangsstelle sollte hinter HTTPS stehen (KB-56), weil Gerätezugänge im Kopf jeder Anfrage stehen.
- Geprüft auf einem Rechner (127.0.0.1) mit `tests/resi.rs`; ein echter Server über das Internet und ein echtes Plattform-Ziel sind nicht geprüft.
