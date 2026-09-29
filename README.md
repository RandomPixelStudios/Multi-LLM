# Multi LLM — Website

The static site for [Multi LLM](https://github.com/RandomPixelStudios/Multi-LLM):
landing page, documentation, legal notices, and the update manifest the desktop
app fetches at start-up.

No build step, no framework, no tracking. Plain HTML, CSS and one JavaScript
file, served by GitHub Pages.

---

## What is here

| File | Purpose |
|---|---|
| `index.html` | Hero, download button, clickable app demo, trailer, provider marquee |
| `docs.html` | Full documentation — installation, tutorials, API reference, Docker guide, FAQ |
| `legal.html` | Imprint, privacy policy, terms (English and German) |
| `update.json` | Update manifest for the desktop app |
| `styles.css` | Site and app-replica styles |
| `script.js` | Dropdown, theme, consent banner and the app demo |
| `assets/logo.png` | App logo |
| `assets/logos/` | Provider logos used in the picker and cards |
| `.nojekyll` | Tells Pages to serve the files as-is |
| `deploy.sh` | Commit and push helper |

---

## Local preview

```bash
cd ~/Schreibtisch/Website
python3 -m http.server 8080
# → http://localhost:8080
```

No install, no dependencies.

### Checks

```bash
node --check script.js
python3 -c "from html.parser import HTMLParser; HTMLParser().feed(open('index.html').read())"
```

---

## Publishing

This directory is the **root of the `Website` branch** — GitHub Pages serves a
branch root, not a subdirectory, so `index.html` has to sit next to
`assets/`. `.nojekyll` keeps Pages from running anything through Jekyll.

```bash
./deploy.sh "what changed"
```

Pages rebuilds automatically, usually within 30–60 seconds. Live at
<https://randompixelstudios.github.io/Multi-LLM/>.

---

## Update manifest

`update.json` is what the desktop app fetches to find out whether a newer
version exists. It is generated from `package.json` in the app repository, not
written by hand:

```bash
# in the app repo
npm run update-manifest          # writes Website/update.json
npm run update-manifest:check    # fails if it drifted
```

The Linux release job runs this automatically and commits the result to this
branch, so a release and the manifest can never disagree.

Shape:

```json
{
  "version": "1.0.5",
  "download_url": "https://github.com/RandomPixelStudios/Multi-LLM/releases",
  "changelog": "…"
}
```

Only a version, a link and a note. Nothing about the visitor's machine is
involved.

---

## Design notes

**Bilingual.** German and English, switched from the footer, remembered in
local storage. Default follows the browser language.

**Dark by default.** The page renders dark on first paint via a small inline
script in the `<head>`, so there is no white flash on a phone with a light OS.

**The demo is a real replica.** Not a screenshot: a faithful, clickable copy of
the desktop UI — every tab works, dialogs open, settings toggle. It runs on
sample data and connects to nothing. On screens up to 900 px it is hidden and
the trailer video takes its place, because the panes become unusable slivers at
that width.

**Consent banner.** GitHub Pages writes server logs (IP address, timestamp,
page, status) before any page of ours can ask anything, so a stored "accepted"
would claim a consent nobody was asked for. The banner therefore appears on
every visit, stores nothing, and the Datenschutz text says plainly that
declining cannot stop the logs. Buttons close the banner; they do not pretend
to change what GitHub already recorded.

**Video.** Embedded from `youtube-nocookie.com` with `loading="lazy"`, so no
cookie is set before the visitor presses play.

---

## Legal

Imprint, privacy policy and terms live in `legal.html` and are reachable from
every page footer. The German version is binding for the German site; both
languages are maintained.

Not legal advice — this is a private, non-commercial project, and the texts
are written to the extent a single operator can comply with them.

## Licence

The website is part of the Multi LLM project. All rights reserved — see
[LICENSE](https://github.com/RandomPixelStudios/Multi-LLM/blob/Windows/LICENSE).
Third-party assets keep their own licences; provider logos and trademarks
belong to their respective owners.
