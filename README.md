# Multi LLM

**All LLM providers. One server. One coding CLI.**

Multi LLM 2.0 runs as a **Docker container** with a web UI: it sits between your
tools and every LLM provider you have access to, speaks OpenAI on the outside
and whatever each provider speaks on the inside. The new **`multillm` CLI** is a
coding agent for the terminal (Linux, Windows, macOS) that signs in to your
server with the same account as the web UI.

> 2.0 replaces the desktop app (Tauri). Data folders of the previous Docker / `--serve` mode keep working.

---

## Quick start (server)

```bash
git clone https://github.com/RandomPixelStudios/Multi-LLM.git
cd Multi-LLM
docker compose up -d
```

Open `http://<host>:5000` and sign in as **Admin** / **admin**. You are asked to
set a new password right away (like a fresh Windows account).

The compose file starts two containers:

| Service | Purpose |
|---|---|
| `multillm` | Web UI, management API, OpenAI-compatible API on port 5000 |
| `searxng`  | Private web search for the CLI (internal only, not published) |

Forgot the admin password? `docker exec -it multillm multillm-server --reset-admin`
resets it to `admin` (must be changed at the next sign-in).

## CLI

```bash
# Linux / macOS
curl -fsSL https://raw.githubusercontent.com/RandomPixelStudios/Multi-LLM/HEAD/install.sh | sh
# Windows (PowerShell)
irm https://raw.githubusercontent.com/RandomPixelStudios/Multi-LLM/HEAD/install.ps1 | iex
```

Run `multillm`, enter the IP or host name of your server and sign in. All models
of your account are available right away.

| Command | |
|---|---|
| `/model` | switch model (all enabled models, virtual bundles, `multillm` auto-routing) |
| `/usage` | usage per model + 5 h / weekly / monthly / token limits, live quotas of OAuth providers |
| `/connect` | add a provider (OAuth sign-in or API key) |
| `/yolo` | yolo mode on/off - **on by default**: no questions, except for clearly dangerous commands |
| `/bypass-permissions` | run *everything* without asking |
| `/ask` | confirm every change and command |
| `/compact`, `/clear` | shrink / reset the conversation |
| `/login`, `/logout`, `/host`, `/status`, `/help` | |

Tools of the agent: `bash` (PowerShell on Windows), `read_file`, `write_file`,
`edit_file`, `glob`, `grep`, `web_fetch` and `web_search` (SearXNG on the server).
`@path` in a prompt attaches a file; `MULTILLM.md` / `AGENTS.md` in the project
are loaded as instructions. One-shot: `multillm -p "fix the failing test"`.

## Features

| | |
|---|---|
| **Accounts** | Windows-style sign-in screen, default `Admin`/`admin` with forced change, argon2id, account lockout (5 wrong passwords -> 5 min), per-IP throttle, sessions per device, sign out everywhere |
| **OAuth providers** | GitHub Copilot (device flow), OpenRouter (official PKCE), Qwen Code (device flow) - no API keys needed |
| **Limits** | rolling 5 h, 7 day and monthly token limits, request caps per 5 h / day - enforced in routing; providers report their real quotas (Copilot premium requests, OpenRouter credits) |
| **Routing** | auto (health + latency + quota headroom), weighted, round robin, quality, latency, fastest, cheapest, least-used (spreads subscriptions), sticky; virtual bundles; circuit breaker + failover |
| **Analytics** | time series (1 h - 30 days), per provider / model: requests, tokens, cost, error rate, p50/p95 latency, comparison with the previous period |
| **API** | `/v1/chat/completions`, `/v1/responses`, `/v1/models`; OpenAI <-> Anthropic translation; `provider::model` pins one provider |

## Configuration

| Variable | Default | |
|---|---|---|
| `MULTI_LLM_PORT` | `5000` | listening port |
| `MULTI_LLM_DATA_DIR` | `/data` | accounts, keys, usage (Docker volume) |
| `MULTI_LLM_SEARXNG_URL` | `http://searxng:8080` | web search backend |
| `MULTI_LLM_PUBLIC_URL` | – | public URL (OAuth redirects behind a reverse proxy) |
| `MULTI_LLM_COOKIE_SECURE` | auto | force `Secure` cookies (TLS proxy) |
| `MULTI_LLM_TRUST_PROXY` | `0` | honour `X-Forwarded-For` for throttling |
| `MULTI_LLM_HIDE_USERS` | `0` | hide user tiles on the sign-in screen |
| `MULTI_LLM_ALLOW_SIGNUP` | `0` | let anyone create an account (default: admins only) |

Behind a reverse proxy with TLS set `MULTI_LLM_COOKIE_SECURE=1`,
`MULTI_LLM_TRUST_PROXY=1` and `MULTI_LLM_PUBLIC_URL=https://…`.

## Development

```bash
cargo test --workspace
cargo run -p multillm-server        # http://localhost:5000
cargo run -p multillm               # CLI
```

```
server/   Rust server (axum): proxy, routing, accounts, OAuth, limits, web UI (server/web)
cli/      multillm coding agent (Rust, single static binary)
docker/   SearXNG configuration
```

Releases: pushing a tag `v*` builds the CLI for Linux (x86_64/aarch64, static),
Windows and macOS and pushes `ghcr.io/randompixelstudios/multi-llm`.

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

Proprietary. You may install and use Multi LLM freely, including in
commercial settings. You may not resell it, redistribute it, publish the
source code, or use the name and logo for your own product — see
[LICENSE](LICENSE) for the full terms. Third-party packages keep their own
licences; see the lock files.
