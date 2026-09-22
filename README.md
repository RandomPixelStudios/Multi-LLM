# MultiLLM

Ein lokaler LLM-Proxy mit Weboberfläche: MultiLLM nimmt OpenAI-kompatible
Anfragen entgegen, verteilt sie über mehrere Provider/Modelle (Routing,
Circuit-Breaker, Health-Checks) und übersetzt zwischen den API-Formaten
(OpenAI, Anthropic, OpenAI *Responses*). Als Tauri-Desktop-App, headless
oder als Multi-User-Server (ein Port für viele Konten) nutzbar.

## Funktionen

- **Ein Endpunkt, viele Provider** – `/v1/responses` und `/v1/*` sprechen
  OpenAI-Format, unabhängig davon, ob oben drin OpenAI, Anthropic, Gemini,
  Ollama/LM Studio/vLLM (lokal) oder ~90 weitere Anbieter liegen.
- **Routing mit Ausfallschutz** – gewichtete/zufällige/latenzbasierte
  Strategie, Circuit-Breaker, periodische Health-Checks, Sticky-Sessions,
  Failover-Budget pro Anfrage.
- **Kontext-Compression & Response-Cache** – lange Prompts werden eingedampft,
  identische Antworten cached (TTL-Regeln konfigurierbar).
- **Usage-Tracking** – pro Modell/Tag, inkl. Export.
- **Multi-User-Server** – ein Prozess, ein Port; jedes Konto hat eigenes
  Profil, eigene Provider/Models/Keys und eigene Usage-Daten.
- **Zwei Oberflächen** – Tauri-Desktop-App (`src/`) und eingebettetes
  Web-Frontend (`src-tauri/web/`, läuft auch ohne Desktop-Hülle).

## Projektstruktur

```
src/                    Desktop-Frontend (TypeScript/Vite, Tauri-IPC)
src/provider-presets.json  Gemeinsame Quelle für die Provider-Presets
src-tauri/src/          Rust-Backend
  proxy.rs              Kern: Routing, Übersetzer, Usage, Sessions, Handler
  server.rs             Multi-User-Registry + Server-Router
  users.rs              Konten, argon2id-Passwörter, Ports
  settings.rs           Config-Schema, Persistenz, Secrets
src-tauri/web/          Eingebettetes Web-Frontend (wird via include_str! gebettet)
scripts/                Hilfs-Skripte (presets-Generator, Windows-Setup)
docker/                 Dockerfile + docker-compose.yml
Website V1/, Trailer/   Archivierte Website-/Trailer-Materialien (nicht Teil des Builds)
```

## Schnellstart

### Desktop-App

```bash
npm install
npm run dev:tauri       # Entwicklungsmodus
npm run build:tauri     # Produktions-Build
```

### Multi-User-Server (ein Port, viele Konten)

```bash
npm run build                       # Frontend bauen (dist/)
cd src-tauri && cargo build --release
./target/release/multi-llm --serve  # Multi-User-Modus
./target/release/multi-llm --headless  # Einzelnutzer ohne Fenster
```

Beim ersten Start ist die Registrierung offen, bis das erste Konto existiert
(Bootstrap). Danach ist Sign-up zu – siehe Tabelle unten.

### Docker

```bash
cd docker
docker compose up --build -d
docker compose ps          # "healthy", sobald der Port gebunden ist
# -> http://localhost:5000  (Login Pflicht)
```

Alle Benutzerdaten liegen im Volume `multillm-data` (`/data`), Prozesse laufen
als Nicht-Root-User `appuser`.

## Konfiguration (Umgebungsvariablen)

| Variable | Standard | Bedeutung |
| --- | --- | --- |
| `MULTI_LLM_PORT` | `5000` | Port des eingebetteten HTTP-Servers |
| `MULTI_LLM_EXPOSE` | `0` | `1` = auch außerhalb von localhost lauschen (LAN/Docker) |
| `MULTI_LLM_SERVER` | `0` | `1` = Single-Port-Multi-User-Modus |
| `MULTI_LLM_DATA_DIR` | plattformabhängig | Ordner für Settings/Secrets/Usage/Konten |
| `MULTI_LLM_ALLOW_SIGNUP` | (Server: zu) | `1` erlaubt Registrierung ohne Admin-Session |
| `MULTI_LLM_COOKIE_SECURE` | `0` | `1` setzt `Secure` am Session-Cookie (HTTPS) |
| `MULTI_LLM_PUBLIC_DASHBOARD` | `0` | `1` erlaubt öffentlichen *read-only* Dashboard-Zugriff |
| `MULTI_LLM_USER_PORT_BASE` | `8123` | Erster persönlicher Port pro Konto |

## Sicherheit

- **Passwörter**: argon2id (PHC-String, 128-Bit-Salt pro Hash). Konten aus
  älteren Releases (`v1$…` iteriertes SHA-256) bleiben lesbar und werden beim
  nächsten Login automatisch migriert. Mindestlänge: 10 Zeichen.
- **Sessions**: In-Memory-Token mit Idle-TTL (12 h), Hard-Max-Age (7 Tagen),
  Größen-Cap mit LRU-Eviction und periodischem Aufräumen. `HttpOnly` +
  `SameSite=Lax`, `Secure` auf Wunsch (`MULTI_LLM_COOKIE_SECURE` bzw.
  `x-forwarded-proto: https`).
- **Sign-up-Gate (Server-Modus)**: Standardmäßig nur, solange es kein Konto
  gibt, mit Admin-Session oder mit `MULTI_LLM_ALLOW_SIGNUP=1`.
- **Zugriffskontrolle**: Benutzerverwaltung nur über die Admin-Session;
  API-Key-Vergleiche laufen konstantzeitig (`ct_eq`).
- **Throttling**: Fehlversuche bei Login/Sign-up/Löschen sind pro IP gedrosselt
  (10 Versuche/Minute, dann 60 s Sperre).
- **Body-Limits**: 8 MB für Verwaltungs-Routen, 64 MB nur für `/v1`.

## Entwicklung

```bash
npm run dev            # Vite-Devserver für das Desktop-Frontend
npm run build          # tsc --noEmit + vite build
npm run check:web      # Syntax-Check aller eingebetteten Web-Skripte
npm run presets        # src-tauri/web/presets.js aus src/provider-presets.json erzeugen
npm run presets:check  # nur prüfen, ob presets.js aktuell ist (Exit 1 bei Drift)

cd src-tauri
cargo test             # Backend-Tests
cargo clippy --all-targets
cargo fmt --all
```

Die Provider-Presets haben **eine** Quelle (`src/provider-presets.json`);
das Desktop-Frontend importiert sie direkt, `scripts/gen-presets.mjs` schreibt
die Kopie für das eingebettete Web-Frontend. Nie eine der beiden Dateien von
Hand bearbeiten.

## Tests & CI

- `cargo test` – Backend (Routing, Übersetzer, Sessions, Persistenz, Konten).
- `npm run build` – Typ-Check + Bundle des Desktop-Frontends.
- `npm run check:web` / `npm run presets:check` – eingebettetes Web-Frontend.
- `.github/workflows/ci.yml` führt das alles bei jedem Push aus.

## Lizenz

MIT – siehe [LICENSE](LICENSE).
