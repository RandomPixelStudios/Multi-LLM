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
| `docs.html` | Full documentation — installation, tutorials, API reference, FAQ |
| `legal.html` | Imprint, privacy policy, terms (English and German) |
| `update.json` | Update manifest for the desktop app |
| `styles.css` | Site and app-replica styles, including the local `@font-face` rules |
| `docs.css` | Documentation layout, layered on top of `styles.css` |
| `docs-common.js` | Shared chrome: topbar, sidebar, footer, search, scroll spy, consent banner |
| `theme-boot.js` | Applies the stored theme before the first paint, prevents a flash |
| `legal-page.js` | Language switcher for `legal.html` only |
| `script.js` | Landing page: dropdown, theme, and the app demo |
| `assets/logo.png` | App logo |
| `assets/logos/` | Provider logos, WebP where they exist, PNG as fallback |
| `assets/fonts/` | Inter and JetBrains Mono (variable, latin subset), SIL OFL 1.1 |
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

**Dark by default.** The page renders dark on first paint via `theme-boot.js`,
loaded with `blocking="render"`, so there is no white flash on a phone with a
light OS.

**Fonts are served from here.** Inter and JetBrains Mono live in `assets/fonts/`
as variable fonts in the latin subset (86 KB together, six weights in two
files). No request ever reaches Google, which is why the consent banner has
nothing to ask about fonts. Both are SIL OFL 1.1; the licence texts sit next to
the files.

**Logos.** Provider logos are WebP where a WebP exists, capped at 56 px — twice
the largest render size, so they stay sharp on a retina screen. `logoHtml()`
tries `.webp` first and falls back to the original file through `onerror`, so
adding a PNG later needs no code change.

**The demo is a real replica.** Not a screenshot: a faithful, clickable copy of
the desktop UI — every tab works, dialogs open, settings toggle. It runs on
sample data and connects to nothing. On screens up to 900 px it is hidden and
the trailer video takes its place, because the panes become unusable slivers at
that width.

**Consent banner.** Only one thing still comes from a third party: the YouTube
player. Before a decision nothing is requested from outside, the video slot
shows a placeholder, and the fonts come from this repository. Choosing "accept
all" loads the player, "necessary only" keeps it out for good. The decision is
one value in local storage, withdrawable any time through "Privacy settings" in
the footer, which reopens the banner and unloads the player on the spot.

**GitHub Pages logs anyway.** The server writes IP address, timestamp, page and
status before any page of ours can ask anything. No button here can prevent
that, so the banner and the privacy policy say so plainly instead of pretending
a choice covers it.

**Content-Security-Policy.** Delivered as a `<meta>` tag, because Pages serves
no custom headers. `base-uri 'none'`, `object-src 'none'` and
`form-action 'self'` do the real work; `script-src` and `style-src` still carry
`'unsafe-inline'` for the inline handlers in the app demo. `frame-ancestors`
would be the strongest addition, but it is ignored in a `<meta>` tag.

---


## Responsibility and liability

Multi LLM routes your requests to third-party language models. It does not
control what those models answer and cannot know in advance what they will say.
**You are solely responsible for how you use this software and for what happens
as a result.**

That includes the content you send through the proxy, the output the models
return (including anything unlawful, misleading, defamatory or harmful that they
produce), compliance with the law in your jurisdiction and with the terms of the
providers you enable, your API keys and the charges they incur, and any decision
you take on the basis of model output.

### Compatibility and environment damage

The author is not liable for damage caused by software or hardware that does not
work with this one, by defects in the environment it runs in, or by faults that
would not have occurred with a different configuration. That includes:

- operating system updates that change a WebView or a runtime library,
- missing or modified system libraries,
- graphics and audio drivers, network stacks, VPNs and proxies,
- antivirus software or firewalls blocking the local port,
- a missing or unstable system tray,
- ports already in use by other programs,
- container or virtualised environments that restrict local ports or paths.

The author is likewise not liable for lost data, lost API keys, lost usage
history or lost configuration; for interrupted, duplicated or corrupted
requests; for responses that arrive incomplete or out of order; for costs a
provider charges for requests that failed; or for damage arising from continued
use despite an error message.

Test this software before you depend on it, keep backups of your configuration,
and check the required system libraries before reporting a problem.

We are not liable for the content of any model response, for the actions or
omissions of any provider, for provider charges, or for any damage arising from
your use of this software. **Decisions with legal, financial, medical or safety
consequences must never be based on model output alone.**

The full wording is in [legal.html](legal.html#terms), section 4.

## Legal

Imprint, privacy policy and terms live in `legal.html` and are reachable from
every page footer. The German version is binding for the German site; both
languages are maintained.

Not legal advice — this is a private, non-commercial project, and the texts
are written to the extent a single operator can comply with them.

## Licence

The website is part of the Multi LLM project. Multi LLM itself may be
installed and used freely, including in commercial settings; you may not
resell it, redistribute it, publish the source code, or use the name and
logo for your own product. See
[LICENSE](https://github.com/RandomPixelStudios/Multi-LLM/blob/Windows/LICENSE).
Third-party assets keep their own licences; provider logos and trademarks
belong to their respective owners.
