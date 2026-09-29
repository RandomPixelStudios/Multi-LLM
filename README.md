# Multi LLM

**All LLM providers. One API key.**

Multi LLM is a local proxy that sits between your tools and every LLM provider
you have keys for. It speaks OpenAI on the outside, speaks whatever each
provider speaks on the inside, and keeps your keys on your own machine.

Point Cursor, VS Code, Continue, OpenCode or any OpenAI-compatible client at
`http://localhost:5000/v1` and every model you configured becomes available
through a single URL and a single key.

> **Beta.** Expect breaking changes. See [Status](#status) below.

---

## What it actually does

| | |
|---|---|
| **One endpoint** | `/v1/chat/completions` and `/v1/responses` talk to 91 cloud and local providers |
| **Format translation** | OpenAI ↔ Anthropic ↔ OpenAI *Responses* — your client never sees the difference |
| **Smart routing** | Weighted, round-robin, priority, latency, fastest or sticky — switchable per virtual model |
| **Automatic failover** | Circuit breakers park failing providers, health checks bring them back |
| **Virtual models** | Group models into a bundle; the proxy picks per request using your strategy |
| **Cost control** | Token-compress prompts (1–10) and per-model token ranking with USD estimates |
| **Private by design** | Keys live in a local file, never leave the machine, no telemetry, no account |
| **Response cache** | Identical answers served from cache with configurable TTL |

---

## Install

Grab the package for your platform from
[GitHub Releases](https://github.com/RandomPixelStudios/Multi-LLM/releases).

| Platform | File | Install |
|---|---|---|
| Debian/Ubuntu/Mint | `.deb` | `sudo apt install ./multi-llm_*_amd64.deb` |
| Fedora/RHEL/openSUSE | `.rpm` | `sudo dnf install ./multi-llm-*.rpm` |
| Any x86_64 | `.AppImage` | `chmod +x *.AppImage && ./multillm.AppImage` |
| Windows | `.msi` or `.exe` | See the `Windows` branch |
| macOS | `.dmg` | This branch, or `Windows` (both build it) |

Runtime dependencies: `libwebkit2gtk-4.1` and `libayatana-appindicator3-1`
(plus `libgtk-3`). Debian 12+, Ubuntu 22.04+, Fedora 38+.

First launch registers a user (that account becomes the admin), then you add
providers and keys under **Providers**.

### Connect a client

```
Base URL:  http://localhost:5000/v1
API key:   shown under API → API Keys
```

In most clients this is two fields. No plugin, no lock-in — if a tool speaks
OpenAI, it works.

---

## How routing works

You do not have to choose a model per request. Define a **virtual model** —
a named bundle of real models — and let the proxy pick:

- **weighted** – random, but starred models get 4× the weight
- **round_robin** – even rotation
- **priority** – first healthy model in provider order
- **latency** – lowest measured average
- **fastest** – latency weighted by current health
- **sticky** – keep sending a conversation to the model that started it

When a provider fails, the circuit breaker opens after 3 consecutive errors
and routing skips it for 60 seconds. Health probes run continuously and close
the breaker as soon as the provider recovers.

---

## Configuration

### Environment variables (server mode)

| Variable | Default | Meaning |
|---|---|---|
| `MULTI_LLM_PORT` | `5000` | Port of the embedded HTTP server |
| `MULTI_LLM_EXPOSE` | `0` | `1` binds to `0.0.0.0` instead of localhost only |
| `MULTI_LLM_SERVER` | `0` | `1` enables single-port multi-user mode |
| `MULTI_LLM_DATA_DIR` | platform default | Where settings, secrets and usage live |
| `MULTI_LLM_ALLOW_SIGNUP` | off in server mode | `1` permits registration without an admin session |
| `MULTI_LLM_COOKIE_SECURE` | `0` | `1` sets `Secure` on the session cookie |
| `MULTI_LLM_PUBLIC_DASHBOARD` | `0` | `1` serves the read-only dashboard without a key |
| `MULTI_LLM_USER_PORT_BASE` | `8123` | First personal port per account |
| `MULTI_LLM_DAILY_BUDGET_USD` | unset | Warns in Usage at 80 % of the limit |

### Running without the UI

```bash
multi-llm --headless   # single user, no window
multi-llm --serve      # multi-user: one port, isolated profiles per account
```

---

---

## Building from source

Requires Node 20+, Rust (stable) and the [Tauri prerequisites](https://tauri.app/start/prerequisites/)
(`libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev` on Debian/Ubuntu).

```bash
npm install
npm run dev              # frontend dev server
npm run build:tauri      # production bundle for this platform
```

### Checks

```bash
npm run build            # tsc --noEmit + vite build
npm run check:web        # syntax check of the embedded web frontend
npm run presets:check    # src-tauri/web/presets.js in sync with the source
npm run legal:check      # src-tauri/web/legal.js in sync with src/legal.ts
npm run update-manifest:check

cd src-tauri
cargo test               # backend: routing, translation, sessions, accounts
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

The provider presets and the legal texts each have exactly one source. The
desktop app imports them directly; `scripts/gen-presets.mjs` and
`scripts/gen-legal.mjs` write the copies for the embedded web frontend, which
cannot load TypeScript. Edit the source, run the generator, never the copy.

### Releases

`.github/workflows/release.yml` builds per branch: `Windows` produces the MSI
and NSIS installer plus a macOS DMG, `mac` produces the DMGs, `Linux` produces
deb/rpm/AppImage and updates the update manifest. Every release is created as a
**draft** — publish it deliberately.

---

## Updates

The app checks a small JSON manifest at start-up (Settings → App → *Check for
updates automatically*, switchable). The manifest lives on the project
website; a self-hosted fork can point the field **Update server** at its own
URL. Nothing about your machine, providers or usage is ever sent — only your
IP address reaches GitHub, which serves the file.

---

## Security

- **Passwords** — argon2id, per-hash 128-bit salt. Legacy `v1$…` hashes (iterated
  SHA-256) still verify and migrate on next login. Minimum 10 characters.
- **Sessions** — in-memory tokens, 12 h idle TTL, 7 d hard max, size-capped with
  LRU eviction. `HttpOnly` + `SameSite=Lax`, `Secure` on request.
- **Key comparison** — constant-time (`ct_eq`) everywhere.
- **Throttling** — 10 failed login/signup attempts per minute per IP, then a
  60-second block.
- **Body limits** — 8 MB for management routes, 64 MB for `/v1`.
- **API keys** — stored locally, compared as hashes, never written to exports
  unless you explicitly include them.

---

## Project layout

```
src/                    Desktop frontend (TypeScript / Vite, Tauri IPC)
  provider-presets.json   Single source for the 91 providers
  legal.ts                Single source for the legal texts
src-tauri/src/          Rust backend
  proxy/                  Routing, format translation, usage, sessions
  server.rs               Multi-user registry and router
  users.rs                Accounts, argon2id passwords, ports
  settings.rs             Config schema, persistence, secrets
  update_check.rs         Update manifest fetch
src-tauri/web/          Embedded web frontend (embedded via include_str!)
scripts/                Generators and setup helpers
```

---

## Status

Beta. The API shape may change. Provider behaviour — availability, pricing,
rate limits — is outside our control. You pay your providers directly; Multi
LLM never bills you.

Legal notices, privacy policy and terms: shipped in the app under
**Settings → About & legal**, and online at
[randompixelstudios.github.io/Multi-LLM/legal.html](https://randompixelstudios.github.io/Multi-LLM/legal.html).


## Responsibility and liability

Multi LLM routes your requests to third-party language models. It does not
control what those models answer and cannot know in advance what they will say.
**You are solely responsible for how you use this software and for what happens
as a result.**

That includes:

- the content you send through the proxy and the prompts you build with it,
- the output the models return — including anything unlawful, misleading,
  defamatory or harmful that they produce,
- compliance with the law in your jurisdiction and with the terms of the
  providers you enable,
- your API keys and the charges they incur,
- any decision you take or action you perform on the basis of model output.

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
omissions of any provider, for costs charged by providers, or for any damage
arising from your use of this software. **Decisions with legal, financial,
medical or safety consequences must never be based on model output alone.**

Content, documentation and downloads are provided "as is", without warranty of
accuracy, completeness, timeliness or fitness for a purpose. Nothing limits
liability that cannot legally be limited (intent and gross negligence, injury to
life, body or health, or mandatory product-liability rules).

Full terms: [legal.html](https://github.com/RandomPixelStudios/Multi-LLM/blob/Windows/legal.html)
— shipped in the app under **Settings → About & legal**.

## Licence

Proprietary. All rights reserved — see [LICENSE](LICENSE). Third-party
packages keep their own licences; see the lock files.
