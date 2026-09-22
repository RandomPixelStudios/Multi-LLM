# Multi LLM – Website V1

Bilingual (DE/EN via footer flag switcher), static, no build step, no tracking.

## Start

```bash
cd "Website V1"
python3 -m http.server 8080
# → http://localhost:8080
```

## Structure

- **Hero**: two-line gradient title, Download button + system dropdown (Windows / Linux / Docker – auto-detects Windows/Linux, selection only, nothing is downloaded), nothing else.
- **Footer**: `MULTILLM` + Flaggen-Sprachdropdown + runder Mond/Sonne-Theme-Toggle + Legal-Links (kein Tagline, kein Prefs-Button; Dialog nur beim Erstbesuch).
- **First-visit dialog**: custom cookie/theme window – Light / Dark / System choice (System preselected), cookie consent below (Accept persists to local storage, Decline closes session-only). Reopenable via footer “Cookie & theme settings”. Previous `ml-theme` value is migrated silently.
- **Full-size app demo** (`#demo`): faithful, fully clickable replica of the desktop UI from `../index.html` + `../src/main.ts` + `../src/ui.css`:
  - Frameless titlebar, sidebar (Models / Providers / Virtual Models / API / Usage / Settings, v1.0.5)
  - Models: search, 4 sorts, star toggles (★ ×4), auto aliases (`o-…`), tok/s + ctx badges, empty state
  - Providers: health dots, enable counts, no-key badge, double-click delete, latency test dialog; Add/Edit/Fetch are locked with an “Install the app” notice
  - Virtual Models: system bundle `multillm` + custom bundles, create dialog, test, double-click delete
  - API: `http://localhost:PORT/v1` pill + copy, port field (live preview), visible demo key with copy/show/double-click reroll, switches locked with notice, client snippets dialog
  - Usage: search, Today/Month/All Time + 5 sorts menu, 4 stats, animated ranking bars
  - Settings: Appearance (theme switch synced with site, working tab order) + Token compression only
- **Providers marquee**: “Works with every provider.” + infinite loop of all 91 presets + Custom.
- **Docs** (`docs.html`, DE+EN, ~75 Sektionen je Sprache: Installation, 21 Tutorials, Konzepte + Glossar, 10 Guides, API-Referenz + Kochbuch (curl/jq/Node/Go/PowerShell/Bash), 15 Docker-Seiten, 20 FAQ): links Nav. Inhalte aus der App (kein Source); Code-Blöcke je Sprache.
- **Why MultiLLM?**: Titel im Hero-Stil + 6 Karten (DE+EN).
- **Legal** (`legal.html`, eigene Unterseite, Footer-Links Impressum/Datenschutz/Bedingungen): projektbezogene Template-Texte (keine Rechtsberatung!) je DE + EN mit gelb markierten `<mark>`-Platzhaltern nur für Name/Adresse/Mail/Telefon/Steuer/Register/Datum/Land/Hoster – Impressum nach § 5 DDG + MStV + OS-Plattform + Schlichtungs-Hinweis, Privacy mit Website-vs-App-Trennung (Keys nur lokal), Hosting-Logs, Local-Storage statt Cookies, Google Fonts, YouTube/Discord-Links, DSGVO-Rechte + CCPA-Hinweis, Terms mit Haftungsausschluss (Beta, Demo-Daten, keine Gewähr, externe Links, Marken, Recht, Salvatorik). Gleiches Topbar-/Footer-/Prefs-/Scrollspy-Setup wie Docs.
- **Sprache**: `ml-lang` in Local Storage (Default = Browser-Sprache). Statik via `<span class="lang-en/lang-de">` + CSS (`html[lang]`), Dynamik (Titel, Meta, Toasts, Dialoge) via `STR`-Dict. Demo-App bleibt Englisch (wie die echte App).
- All demo data is local to the page – nothing connects anywhere.

## Files

| File | Purpose |
|---|---|
| `index.html` | Hero + app replica markup |
| `styles.css` | Site + replica styles (mirrors `../src/ui.css` tokens) |
| `script.js` | Dropdown + full demo logic |
| `assets/logo.png` | App logo |
| `assets/logos/` | Provider logos used in picker/cards |

## Checks

```bash
node --check "Website V1/script.js"
python3 -c "from html.parser import HTMLParser; HTMLParser().feed(open('Website V1/index.html').read())"
```
