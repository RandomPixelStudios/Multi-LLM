# Multi LLM als Docker-App (Multi-User, ein Port)

Der Container baut Frontend + Backend nach und startet im
**Multi-User-Modus** (`--serve`):

- **Ein Port für alle: 5000** (fest, nicht änderbar).
- **Ohne Login sieht man nichts** – Start zeigt nur die Benutzer-Auswahl
  wie am Windows-Login (Kachel anklicken, Passwort, fertig).
- Nach dem Login siehst du **nur deine** Models, Provider, Usage und
  Settings. Abmelden per Button (oder Browser schließen – die Session
  endet damit).
- Jede API-Anfrage (`/v1/*`) wird **automatisch per API-Key dem richtigen
  Benutzer zugeordnet** – alle arbeiten gleichzeitig über denselben Port.
- Der **API-Key wird pro Benutzer automatisch erstellt und bleibt immer
  gleich** (Anzeige + Kopieren im API-Tab, kein Erneuern).
- Den **Benutzer-Tab sieht nur der erste Benutzer (Admin)** – dort lassen
  sich Konten anlegen und löschen. Anlegen geht auch vom Login aus
  (nur Benutzername + Passwort, kein Google/E-Mail).
- `/dashboard` (Übersicht) ist ebenfalls login-pflichtig.

> Hinweis: Das native Tauri-Desktopfenster gibt es im Container nicht.
> Die Web-Verwaltung steuert alles wie die Desktop-App.

## Start

Aus dem `docker/`-Verzeichnis (eine Ebene höher):

```bash
docker compose up --build -d
```

Danach:

- lokal: http://localhost:5000 (Login-Pflicht!)
- andere PCs: `http://<IP-DIESES-RECHNERS>:5000`
  (Firewall: Port 5000 freigeben, z. B. `ufw allow 5000/tcp`)

Erster Start: Benutzer anlegen (wird Admin), danach weitere nach Bedarf.

## Dateien in diesem Ordner

- `README.md` – diese Anleitung (mit auf andere Systeme nehmbar)
- `get-key.sh` – lokalen API-Key eines Benutzers ausgeben

`docker-compose.yml` und `Dockerfile` liegen bewusst **außerhalb**
(direkt in `docker/`), alles zum Starten Nötige sonst hier.
Zum Mitnehmen auf andere Systeme: diesen `docker/`-Ordner **plus das
Repo** (Build braucht den Quellcode) – oder das gebaute Image
(`docker save multillm:latest` / `docker load`).

## Kein Autostart

In `docker-compose.yml` steht ausdrücklich `restart: "no"` – nach einem
Systemstart läuft **nichts** automatisch. Manueller Start/Stopp:

```bash
docker compose start     # starten
docker compose stop      # stoppen (bleibt erhalten)
docker compose down      # stoppen + Container entfernen (Daten bleiben im Volume)
docker compose logs -f   # Logs ansehen
```

Auch bei `docker run` von Hand gilt: Standard ist kein Neustart
(`--restart=no` ist Docker-Default).

## API-Key (für API-Clients)

Pro Benutzer, automatisch erstellt, ändert sich nie:

```bash
./get-key.sh anna
```

In Clients als `Authorization: Bearer <key>` gegen `http://<host>:5000/v1`
nutzen – Anfragen landen automatisch beim richtigen Profil.

## Daten

Alle Daten (Konten, Provider, Models, Keys, Usage) liegen im Volume
`multillm-data` (`/data` im Container, pro Benutzer unter
`/data/users/<name>/`) und überleben Updates:

```bash
docker compose up --build -d   # neu bauen genügt, Daten bleiben
```

## Umgebungsvariablen (Referenz)

| Variable | Default (Compose) | Wirkung |
|---|---|---|
| `MULTI_LLM_DATA_DIR` | `/data` | Datenverzeichnis im Container |
| `MULTI_LLM_PORT` | `5000` | Fester Port (nicht änderbar) |
| `MULTI_LLM_EXPOSE` | `1` | Bind auf `0.0.0.0` statt nur localhost |
| `MULTI_LLM_SERVER` | `1` | Multi-User-Modus (Profile pro Key/Session) |
