#!/usr/bin/env python3
"""Freie Tonaufnahmen aus einer Wikimedia-Commons-Kategorie laden (A3-Korpus).

Nur Dateien mit freier Lizenz (CC0, Public Domain, CC BY, CC BY-SA) werden
übernommen; Quelle, Urheber und Lizenz landen in <ziel>/QUELLEN.tsv.
Jede Datei wird nach <ziel>/<klasse>/commons_<n>.wav (16 kHz mono) gewandelt,
höchstens <max-sekunden> lang.

Aufruf: scripts/vad/commons-laden.py <ziel> <klasse> "<Kategorie>" [max-dateien=40] [max-sekunden=120]
Beispiel: scripts/vad/commons-laden.py ~/Library/Caches/lz-streamer-testdaten/commons orgel "Audio files of organ music"
"""
import json
import os
import re
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

ziel, klasse, kat = sys.argv[1], sys.argv[2], sys.argv[3]
max_n = int(sys.argv[4]) if len(sys.argv) > 4 else 40
max_s = int(sys.argv[5]) if len(sys.argv) > 5 else 120
UA = {"User-Agent": "lz-streamer-korpus/0.1 (Testkorpus)"}
FREI = re.compile(r"^(cc0|public domain|pd|cc[ -]by(-sa)?[ -]?[0-9.]*|cc by(-sa)? [0-9.]+)", re.I)


def holen(url, timeout=120):
    """GET mit Pause und Wiederholung bei 429 (Commons drosselt Massenabrufe)."""
    for versuch in range(6):
        time.sleep(3)
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=timeout) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            if e.code != 429:
                raise
            time.sleep(30 * (versuch + 1))
    raise RuntimeError("429 bleibt")


def api(params):
    url = "https://commons.wikimedia.org/w/api.php?" + urllib.parse.urlencode({**params, "format": "json"})
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=60) as r:
        return json.load(r)


os.makedirs(f"{ziel}/{klasse}", exist_ok=True)
roh = f"{ziel}/roh"
os.makedirs(roh, exist_ok=True)
quellen = f"{ziel}/QUELLEN.tsv"
schon = set()
if os.path.exists(quellen):
    schon = {line.split("\t")[2] for line in open(quellen, encoding="utf-8") if line.count("\t") >= 2}
n = len([f for f in os.listdir(f"{ziel}/{klasse}") if f.startswith("commons_")])
cont = {}
while n < max_n:
    d = api({"action": "query", "generator": "categorymembers", "gcmtitle": f"Category:{kat}", "gcmtype": "file", "gcmlimit": 50,
             "prop": "imageinfo", "iiprop": "url|extmetadata|mime|size", **cont})
    for p in d.get("query", {}).get("pages", {}).values():
        if n >= max_n:
            break
        ii = (p.get("imageinfo") or [{}])[0]
        mime = ii.get("mime", "")
        if not (mime.startswith("audio/") or mime in ("application/ogg", "video/ogg")) or p["title"] in schon:
            continue
        meta = ii.get("extmetadata", {})
        lic = meta.get("LicenseShortName", {}).get("value", "")
        if not FREI.match(lic.strip()):
            continue
        artist = re.sub("<[^>]+>", "", meta.get("Artist", {}).get("value", "")).strip().replace("\t", " ")
        src = f"{roh}/{p['pageid']}"
        try:
            data = holen(ii["url"])
            with open(src, "wb") as f:
                f.write(data)
            out = f"{ziel}/{klasse}/commons_{p['pageid']}.wav"
            subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", src, "-t", str(max_s), "-ac", "1", "-ar", "16000", out], check=True)
        except Exception as e:  # noqa: BLE001 – einzelne Datei überspringen
            print("übersprungen:", p["title"], e, file=sys.stderr)
            continue
        with open(quellen, "a", encoding="utf-8") as q:
            q.write(f"{klasse}\tcommons_{p['pageid']}.wav\t{p['title']}\t{lic}\t{artist}\t{ii.get('descriptionurl', '')}\n")
        n += 1
        print(n, p["title"], lic)
    if "continue" not in d:
        break
    cont = d["continue"]
