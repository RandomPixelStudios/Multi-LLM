# Multi LLM — Official Trailer

Ein cinematischer App-Trailer mit Musik. Die App wird darin **1:1 nachgebaut** —
nicht als Video, sondern als echtes DOM mit dem **originalen Stylesheet der App**
(`../src/ui.css`), inklusive Icon-Map, Karten-Aufbau und Light-Mode.

## Starten

Am einfachsten über einen lokalen Server (wegen der Logos aus `../public/`):

```bash
# vom Projektordner aus
python3 -m http.server 8912
# dann öffnen:  http://localhost:8912/Trailer/index.html
```

Doppelklick auf `index.html` funktioniert in den meisten Browsern ebenfalls.

> Browser-Regel: Audio startet erst nach Klick auf **„Trailer abspielen"**
> (Autoplay-Sperre). Danach läuft alles automatisch.

## Ablauf (~76 Sekunden)

| Zeit | Szene |
|------|-------|
| 0:00 | Logo + Wortmarke |
| 0:04 | Tagline |
| 0:08 | **Models** — Karten laufen gestaffelt ein |
| 0:15 | **Providers** — Presets, Health-Dots |
| 0:21 | **Virtual Models** |
| 0:27 | **API** — Endpunkt, Key, Toggles schalten sich |
| 0:34 | **Usage** — Zähler laufen hoch, Ranking-Balken wachsen |
| 0:41 | **Settings** — Light-Mode-Wechsel + Slider |
| 0:48 | **Docker Multi-User** — Login-Overlay (Port 5000) |
| 0:54 | Windows · Linux · Docker |
| 1:00 | **AVAILABLE NOW** |

## Steuerung

| Taste / Button | Funktion |
|---|---|
| **Leertaste** oder **R** | Neu abspielen |
| **M** | Ton an/aus |
| ↳-Button unten rechts | Replay |
| 🔊-Button unten rechts | Stummschalten |
| Fortschrittsbalken | Abspielfortschritt |

## Dateien

- `index.html` — Bühne + 1:1-App-Rekonstruktion (Titlebar, Sidebar, alle 6 Panels)
- `trailer.css` — cinematische Ebene: Letterbox, Vignette, Filmkorn, Kamerafahrten,
  Lower-Thirds, Endcard. Die App-Darstellung selbst kommt unverändert aus `../src/ui.css`.
- `trailer.js` — Szenen-Timeline, App-Inhalte, Icon-Hydrierung und die
  **Musik-Engine (Web Audio API)**
- `MultiLLM-Trailer.mp4` — gerenderte Fassung, 1600×900, H.264 + AAC, ~1:19, ca. 12 MB

### MP4 neu rendern

Die MP4 entsteht aus der HTML-Fassung (Bildschirmaufnahme + Audio-Capture der
Web-Audio-Bus, dann Mux mit ffmpeg). Ein Render-Skript-Beispiel:

1. `python3 -m http.server 8912` im Projektordner starten
2. Seite in Playwright mit `recordVideo` aufzeichnen, parallel
   `Music.attachCapture()` per `MediaRecorder` als Audio-Stream mitschneiden
3. Muxen:

```bash
ffmpeg -i video.webm -i audio.webm \
  -map 0:v:0 -map 1:a:0 \
  -c:v libx264 -crf 20 -pix_fmt yuv420p -movflags +faststart \
  -c:a aac -b:a 192k -shortest MultiLLM-Trailer.mp4
```

## Musik

Komplett synthetisiert, keine externe Audiodatei und keine Copyright-Frage:

- Akkordfolge **Dm – Bb – F – C**, Tempo 78 BPM
- Layer: Warmer Pad, Klavier-Arpeggio, Sub-Bass, Hi-Hat/Kick, Riser und Impact
- Aufgebaut in vier Abschnitten: *intro → build → lift → climax → resolve*
- Reverb über einen selbst erzeugten Convolver-Impuls
- Whoosh-Effekte sitzen exakt auf den Szenenwechseln

## Hinweis zur 1:1-Nachbildung

Die Panels, Karten (`.mcard`), Usage-Ranking-Farben (`RANK_COLORS`), Icons,
Formatierungen (`tok/s`, `K ctx`, `fmtCompact`) und die Dark-/Light-Variablen
sind identisch zur App übernommen — inklusive der Leerzustands- und
Hover-Regeln aus `src/ui.css`.
