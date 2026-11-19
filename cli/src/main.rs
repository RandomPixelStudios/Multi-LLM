//! multillm - coding agent CLI for a Multi LLM server.
//!
//!   multillm                 interactive session
//!   multillm -p "task"       one-shot (non-interactive)
//!   multillm --host HOST     use / switch the server
//!   multillm --logout        forget the session on this machine

mod agent;
mod client;
mod config;
mod tools;
mod ui;

use agent::{Agent, Mode};
use client::Client;
use crossterm::style::Stylize;
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use serde_json::{json, Value};

const COMMANDS: [(&str, &str); 14] = [
    ("/help", "Diese Hilfe"),
    ("/model", "Modell wechseln"),
    ("/usage", "Nutzung & Limits (5 h / Woche / Monat / Tokens)"),
    ("/connect", "Provider hinzufügen (OAuth oder API-Key)"),
    ("/yolo", "Yolo-Modus an/aus (Standard: an)"),
    ("/bypass-permissions", "Alle Befehle ohne Nachfrage ausführen (an/aus)"),
    ("/ask", "Vor jeder Änderung nachfragen"),
    ("/clear", "Unterhaltung leeren"),
    ("/compact", "Unterhaltung zusammenfassen (spart Kontext)"),
    ("/status", "Verbindung, Modell, Modus, Tokens"),
    ("/login", "Neu anmelden / Benutzer wechseln"),
    ("/logout", "Abmelden"),
    ("/host", "Anderen Multi-LLM-Host verwenden"),
    ("/exit", "Beenden"),
];

struct ReplHelper;
impl Completer for ReplHelper {
    type Candidate = Pair;
    fn complete(&self, line: &str, pos: usize, _: &Context<'_>) -> rustyline::Result<(usize, Vec<Pair>)> {
        if !line.starts_with('/') || line[..pos].contains(' ') {
            return Ok((0, vec![]));
        }
        let c = COMMANDS
            .iter()
            .filter(|(c, _)| c.starts_with(&line[..pos]))
            .map(|(c, d)| Pair { display: format!("{c:<22} {d}"), replacement: c.to_string() })
            .collect();
        Ok((0, c))
    }
}
impl Hinter for ReplHelper {
    type Hint = String;
    fn hint(&self, line: &str, pos: usize, _: &Context<'_>) -> Option<String> {
        if line.len() < 2 || !line.starts_with('/') || pos != line.len() {
            return None;
        }
        COMMANDS.iter().find(|(c, _)| c.starts_with(line) && *c != line).map(|(c, _)| c[line.len()..].to_string())
    }
}
impl Highlighter for ReplHelper {
    fn highlight_hint<'h>(&self, hint: &'h str) -> std::borrow::Cow<'h, str> {
        std::borrow::Cow::Owned(ui::dim(hint))
    }
}
impl Validator for ReplHelper {}
impl Helper for ReplHelper {}

struct Args {
    host: Option<String>,
    prompt: Option<String>,
    logout: bool,
    model: Option<String>,
    mode: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args { host: None, prompt: None, logout: false, model: None, mode: None };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "-h" | "--help" => {
                println!("multillm {} - Coding-Agent für Multi LLM\n", env!("CARGO_PKG_VERSION"));
                println!("  multillm                    interaktive Sitzung");
                println!("  multillm -p \"Aufgabe\"       einmalig ausführen und beenden");
                println!("  multillm --host <IP|Name>   Server festlegen (Standard-Port 5000)");
                println!("  multillm -m <modell>        Modell für diese Sitzung");
                println!("  multillm --ask | --bypass-permissions");
                println!("  multillm --logout           Sitzung auf diesem Gerät löschen");
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("multillm {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--host" => a.host = it.next(),
            "-p" | "--print" => a.prompt = it.next(),
            "-m" | "--model" => a.model = it.next(),
            "--logout" => a.logout = true,
            "--ask" => a.mode = Some("ask".into()),
            "--yolo" => a.mode = Some("yolo".into()),
            "--bypass-permissions" | "--dangerously-skip-permissions" => a.mode = Some("bypass".into()),
            other => {
                // Bare words = prompt for one-shot convenience.
                let mut p = a.prompt.take().unwrap_or_default();
                if !p.is_empty() {
                    p.push(' ');
                }
                p.push_str(other);
                a.prompt = Some(p);
            }
        }
    }
    a
}

/* ================= connection + sign-in ================= */

async fn ask_host(cfg: &mut config::Config) -> Client {
    loop {
        println!("{}", ui::bold("Mit Multi LLM verbinden"));
        let Some(h) = ui::prompt_line(&format!("  {} ", "IP-Adresse oder Hostname des Multi-LLM-Servers:".with(ui::DIM))) else {
            std::process::exit(0);
        };
        if h.trim().is_empty() {
            continue;
        }
        let base = config::normalize_host(&h);
        let c = Client::new(&base);
        let mut sp = ui::Spinner::start(&format!("Verbinde mit {base}"));
        let r = c.server_info().await;
        sp.stop();
        match r {
            Ok(v) => {
                ui::success(&format!("Verbunden mit Multi LLM {} auf {base}", v["version"].as_str().unwrap_or("")));
                cfg.host = Some(base);
                cfg.session = None;
                config::save(cfg);
                return c;
            }
            Err(e) => ui::error(&format!("{base}: {e}")),
        }
    }
}

/// Sign-in like on the web UI: pick a user tile (or "other user"), password,
/// forced password change for fresh accounts.
async fn sign_in(c: &mut Client, cfg: &mut config::Config) -> Value {
    let tiles = c.tiles().await.unwrap_or_default();
    loop {
        let mut items: Vec<ui::Item> = tiles
            .iter()
            .map(|(u, d)| ui::Item { label: d.clone().unwrap_or_else(|| u.clone()), detail: if d.is_some() { u.clone() } else { String::new() } })
            .collect();
        items.push(ui::Item { label: "Anderer Benutzer".into(), detail: String::new() });
        let last = cfg.username.clone().unwrap_or_default();
        let initial = tiles.iter().position(|(u, _)| *u == last).unwrap_or(0);
        println!();
        let username = if tiles.is_empty() {
            ui::prompt_line("  Benutzername: ").unwrap_or_default()
        } else {
            match ui::pick("Anmelden als", &items, initial) {
                Some(i) if i < tiles.len() => {
                    println!("  {} {}", "Benutzer:".with(ui::DIM), tiles[i].0.clone().bold());
                    tiles[i].0.clone()
                }
                Some(_) => ui::prompt_line("  Benutzername: ").unwrap_or_default(),
                None => std::process::exit(0),
            }
        };
        if username.trim().is_empty() {
            continue;
        }
        let Some(pw) = ui::prompt_secret("  Kennwort: ") else { std::process::exit(0) };
        let mut sp = ui::Spinner::start("Anmelden");
        let r = c.login(username.trim(), &pw).await;
        sp.stop();
        match r {
            Ok((tok, user)) => {
                c.session = Some(tok.clone());
                cfg.session = Some(tok);
                cfg.username = user["username"].as_str().map(str::to_string);
                config::save(cfg);
                if user["mustChangePassword"].as_bool() == Some(true) {
                    println!("  {}", ui::warn("Sie müssen Ihr Kennwort ändern, bevor Sie fortfahren."));
                    loop {
                        let a = ui::prompt_secret("  Neues Kennwort: ").unwrap_or_default();
                        let b = ui::prompt_secret("  Kennwort bestätigen: ").unwrap_or_default();
                        if a != b {
                            ui::error("Die Kennwörter stimmen nicht überein.");
                            continue;
                        }
                        match c.change_password(&pw, &a).await {
                            Ok(_) => {
                                ui::success("Kennwort geändert.");
                                break;
                            }
                            Err(e) => ui::error(&e.message),
                        }
                    }
                }
                let name = user["displayName"].as_str().or(user["username"].as_str()).unwrap_or("").to_string();
                ui::success(&format!("Willkommen, {name}"));
                return user;
            }
            Err(e) => {
                if e.status == 401 && !e.message.contains("locked") {
                    ui::error("Das Kennwort ist falsch. Versuchen Sie es erneut.");
                } else {
                    ui::error(&e.message);
                }
            }
        }
    }
}

/// Connect + authenticate (reuses a stored session when still valid).
async fn connect(args: &Args, cfg: &mut config::Config, force_login: bool) -> (Client, Value) {
    if let Some(h) = &args.host {
        let base = config::normalize_host(h);
        if cfg.host.as_deref() != Some(base.as_str()) {
            cfg.host = Some(base);
            cfg.session = None;
            config::save(cfg);
        }
    }
    let mut c = match &cfg.host {
        Some(h) => {
            let c = Client::new(h);
            match c.server_info().await {
                Ok(_) => c,
                Err(e) => {
                    ui::error(&format!("{h}: {e}"));
                    ask_host(cfg).await
                }
            }
        }
        None => ask_host(cfg).await,
    };
    let mut user = Value::Null;
    if !force_login {
        if let Some(s) = cfg.session.clone() {
            c.session = Some(s);
            match c.me().await {
                Ok(v) if v["user"]["mustChangePassword"].as_bool() != Some(true) => user = v["user"].clone(),
                _ => c.session = None,
            }
        }
    }
    if user.is_null() {
        user = sign_in(&mut c, cfg).await;
    }
    if let Err(e) = c.fetch_api_key().await {
        ui::error(&format!("API-Key konnte nicht geladen werden: {e}"));
    }
    (c, user)
}

/* ================= models ================= */

struct ModelChoice {
    id: String,
    label: String,
    detail: String,
}

async fn model_list(c: &Client) -> Vec<ModelChoice> {
    let mut out = vec![ModelChoice { id: "multillm".into(), label: "multillm".into(), detail: "automatisches Routing über alle Modelle".into() }];
    let Ok(v) = c.get("/api/models").await else { return out };
    for vm in v["virtualModels"].as_array().cloned().unwrap_or_default() {
        if let Some(id) = vm["id"].as_str() {
            out.push(ModelChoice { id: id.into(), label: id.into(), detail: format!("virtuell · {} Modelle", vm["models"].as_array().map(|a| a.len()).unwrap_or(0)) });
        }
    }
    for p in v["providers"].as_array().cloned().unwrap_or_default() {
        let pid = p["id"].as_str().unwrap_or("");
        for m in p["models"].as_array().cloned().unwrap_or_default() {
            if m["enabled"].as_bool() != Some(true) {
                continue;
            }
            let mid = m["id"].as_str().unwrap_or("");
            let mut detail = pid.to_string();
            if let Some(cl) = m["contextLength"].as_u64() {
                detail.push_str(&format!(" · {} ctx", ui::fmt_num(cl as f64)));
            }
            if m["starred"].as_bool() == Some(true) {
                detail.push_str(" · ★");
            }
            out.push(ModelChoice { id: format!("{pid}::{mid}"), label: mid.to_string(), detail });
        }
    }
    out
}

fn short_model(id: &str) -> String {
    match id.split_once("::") {
        Some((p, m)) => format!("{m} ({p})"),
        None => id.to_string(),
    }
}

async fn choose_model(agent: &mut Agent, cfg: &mut config::Config) {
    let list = model_list(&agent.client).await;
    if list.len() == 1 {
        ui::info("Noch keine Modelle aktiv – füge mit /connect einen Provider hinzu.");
    }
    let items: Vec<ui::Item> = list
        .iter()
        .map(|m| ui::Item { label: if m.id == agent.model { format!("{} ✓", m.label) } else { m.label.clone() }, detail: m.detail.clone() })
        .collect();
    let cur = list.iter().position(|m| m.id == agent.model).unwrap_or(0);
    if let Some(i) = ui::pick("Modell wählen", &items, cur) {
        agent.model = list[i].id.clone();
        cfg.model = Some(agent.model.clone());
        config::save(cfg);
        ui::success(&format!("Modell: {}", short_model(&agent.model)));
    }
}

/* ================= /usage ================= */

async fn show_usage(agent: &Agent) {
    let c = &agent.client;
    let (lim, an) = tokio::join!(c.get("/api/limits"), c.get("/api/analytics?range=24h"));
    println!();
    println!("  {}", ui::bold("Diese Sitzung"));
    println!("    {} Input · {} Output Tokens", ui::fmt_num(agent.session_in as f64), ui::fmt_num(agent.session_out as f64));
    if let Ok(an) = an {
        println!();
        println!("  {}", ui::bold("Letzte 24 h"));
        let t = &an["totals"];
        println!(
            "    {} Anfragen · {} Tokens · ${:.4} · Fehlerquote {:.1} %",
            ui::fmt_num(t["ok"].as_f64().unwrap_or(0.0)),
            ui::fmt_num(t["inputTokens"].as_f64().unwrap_or(0.0) + t["outputTokens"].as_f64().unwrap_or(0.0)),
            t["costUsd"].as_f64().unwrap_or(0.0),
            t["errorRate"].as_f64().unwrap_or(0.0) * 100.0
        );
        let models = an["models"].as_array().cloned().unwrap_or_default();
        if !models.is_empty() {
            println!();
            println!("    {:<36} {:>8} {:>10} {:>10} {:>9}", ui::dim("Modell"), ui::dim("Anfr."), ui::dim("Input"), ui::dim("Output"), ui::dim("p50"));
            for m in models.iter().take(12) {
                let name: String = format!("{} ({})", m["modelId"].as_str().unwrap_or(""), m["providerId"].as_str().unwrap_or("")).chars().take(36).collect();
                let p50 = m["latencyMs"]["p50"].as_u64().map(|v| format!("{v} ms")).unwrap_or("–".into());
                println!(
                    "    {:<36} {:>8} {:>10} {:>10} {:>9}",
                    name,
                    ui::fmt_num(m["ok"].as_f64().unwrap_or(0.0)),
                    ui::fmt_num(m["inputTokens"].as_f64().unwrap_or(0.0)),
                    ui::fmt_num(m["outputTokens"].as_f64().unwrap_or(0.0)),
                    p50
                );
            }
        }
    }
    match lim {
        Ok(lim) => {
            for p in lim["providers"].as_array().cloned().unwrap_or_default() {
                let pid = p["providerId"].as_str().unwrap_or("");
                let u = &p["usage"];
                println!();
                let tag = p["oauth"].as_str().map(|o| format!(" [{o}]")).unwrap_or_default();
                let ex = if p["exhausted"].as_bool() == Some(true) { format!(" {}", ui::bad("LIMIT ERREICHT")) } else { String::new() };
                println!("  {}{}{}", ui::bold(pid), ui::dim(&tag), ex);
                println!(
                    "    {}  5 h: {}  ·  Woche: {}  ·  Monat: {} Tokens",
                    ui::dim("genutzt"),
                    ui::fmt_num(u["tokens5h"].as_f64().unwrap_or(0.0)),
                    ui::fmt_num(u["tokensWeek"].as_f64().unwrap_or(0.0)),
                    ui::fmt_num(u["tokensMonth"].as_f64().unwrap_or(0.0))
                );
                let mut rows: Vec<Value> = p["remote"].as_array().cloned().unwrap_or_default();
                rows.extend(p["local"].as_array().cloned().unwrap_or_default());
                for l in rows {
                    let label = l["label"].as_str().unwrap_or("");
                    if l["window"] == "info" {
                        println!("    {}", ui::dim(label));
                        continue;
                    }
                    if l["window"] == "error" {
                        println!("    {} {}", ui::warn("!"), ui::dim(label));
                        continue;
                    }
                    if l["unlimited"].as_bool() == Some(true) {
                        println!("    {:<30} {}", label, ui::ok("unbegrenzt"));
                        continue;
                    }
                    let used = l["used"].as_f64().unwrap_or(0.0);
                    let usd = l["unit"] == "usd";
                    let fmtv = |v: f64| if usd { format!("${v:.2}") } else { ui::fmt_num(v) };
                    let pct = l["usedPercent"].as_f64().or_else(|| l["limit"].as_f64().filter(|x| *x > 0.0).map(|lim| used / lim * 100.0));
                    let right = match l["limit"].as_f64() {
                        Some(lim) => format!("{} / {}", fmtv(used), fmtv(lim)),
                        None => fmtv(used),
                    };
                    let reset = l["resetsAtMs"]
                        .as_u64()
                        .map(|ms| {
                            let s = (ms as i64 - chrono::Utc::now().timestamp_millis()).max(0) / 1000;
                            if s >= 86400 { format!(" · Reset in {} d", s / 86400) } else { format!(" · Reset in {} h {} min", s / 3600, (s % 3600) / 60) }
                        })
                        .or_else(|| l["resetsAt"].as_str().map(|d| format!(" · Reset {}", &d[..d.len().min(10)])))
                        .unwrap_or_default();
                    match pct {
                        Some(pc) => println!("    {:<30} {} {:>5.1} %  {}{}", label, ui::bar(pc, 20), pc, ui::dim(&right), ui::dim(&reset)),
                        None => println!("    {:<30} {}{}", label, right, ui::dim(&reset)),
                    }
                }
            }
        }
        Err(e) => ui::error(&e.message),
    }
    println!();
}

/* ================= /connect ================= */

fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    let _ = r;
}

fn slug(s: &str) -> String {
    let base: String = s.split('(').next().unwrap_or(s).to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let mut out = String::new();
    for ch in base.chars() {
        if ch == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        out.push(ch);
    }
    let t = out.trim_end_matches('-').chars().take(40).collect::<String>();
    if t.is_empty() { "provider".into() } else { t }
}

async fn connect_provider(c: &Client) {
    let info = c.get("/api/server-info").await.unwrap_or(Value::Null);
    let presets = c.get("/api/presets").await.unwrap_or(json!([]));
    let oauth = info["oauthProviders"].as_array().cloned().unwrap_or_default();
    let presets = presets.as_array().cloned().unwrap_or_default();
    let mut items: Vec<ui::Item> = oauth
        .iter()
        .map(|o| ui::Item { label: format!("{}  (Anmelden)", o["name"].as_str().unwrap_or("")), detail: format!("OAuth · {}", o["limits"].as_str().unwrap_or("")) })
        .collect();
    items.extend(presets.iter().map(|p| ui::Item { label: p["name"].as_str().unwrap_or("").to_string(), detail: format!("API-Key · {}", p["url"].as_str().unwrap_or("")) }));
    items.push(ui::Item { label: "Eigener Endpunkt …".into(), detail: "OpenAI-/Anthropic-kompatibel".into() });
    let Some(i) = ui::pick("Provider verbinden", &items, 0) else { return };
    if i < oauth.len() {
        let kind = oauth[i]["kind"].as_str().unwrap_or("");
        let r = match c.post("/api/oauth/start", json!({"kind": kind})).await {
            Ok(r) => r,
            Err(e) => return ui::error(&e.message),
        };
        let url = r["url"].as_str().unwrap_or("");
        println!();
        if let Some(code) = r["userCode"].as_str().filter(|s| !s.is_empty()) {
            println!("  1. Öffne {}", url.with(ui::ACCENT).underlined());
            println!("  2. Code eingeben:  {}", code.bold().with(ui::TEAL));
        } else {
            println!("  Im Browser anmelden: {}", url.with(ui::ACCENT).underlined());
            println!("  {}", ui::dim("(die Rückleitung geht an den Multi-LLM-Server – Fenster danach schließen)"));
        }
        open_browser(url);
        let flow = r["flowId"].as_str().unwrap_or("").to_string();
        let mut sp = ui::Spinner::start("Warte auf Anmeldung");
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        let ctrlc = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                let _ = tx.send(true);
            }
        });
        let res = loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {}
                _ = rx.changed() => break Err("abgebrochen".to_string()),
            }
            match c.get(&format!("/api/oauth/status/{flow}")).await {
                Ok(s) if s["status"] == "done" => break Ok(s["message"].as_str().unwrap_or("Verbunden").to_string()),
                Ok(s) if s["status"] == "error" => break Err(s["message"].as_str().unwrap_or("Fehler").to_string()),
                Ok(_) => {}
                Err(e) => break Err(e.message),
            }
        };
        ctrlc.abort();
        sp.stop();
        match res {
            Ok(m) => ui::success(&m),
            Err(e) => ui::error(&e),
        }
        return;
    }
    let (name, url, format) = if i - oauth.len() < presets.len() {
        let p = &presets[i - oauth.len()];
        (p["name"].as_str().unwrap_or("").to_string(), p["url"].as_str().unwrap_or("").to_string(), p["format"].as_str().unwrap_or("openai").to_string())
    } else {
        let u = ui::prompt_line("  Basis-URL (z. B. https://api.example.com/v1): ").unwrap_or_default();
        let f = ui::prompt_line("  Format [openai/anthropic] (openai): ").unwrap_or_default();
        (String::new(), u, if f.trim() == "anthropic" { "anthropic".into() } else { "openai".into() })
    };
    let default_id = slug(if name.is_empty() { &url } else { &name });
    let id = ui::prompt_line(&format!("  ID ({default_id}): ")).unwrap_or_default();
    let id = if id.trim().is_empty() { default_id } else { slug(&id) };
    let key = ui::prompt_secret("  API-Key (leer für keinen): ").unwrap_or_default();
    let key = if key.trim().is_empty() { Value::Null } else { json!(key.trim()) };
    let mut sp = ui::Spinner::start("Lade Modelle");
    let models = c.post("/api/providers/fetch", json!({"baseUrl": url, "apiKey": key, "apiFormat": format})).await;
    sp.stop();
    let models = match models {
        Ok(m) => m.as_array().cloned().unwrap_or_default(),
        Err(e) => {
            ui::error(&format!("Modelle konnten nicht geladen werden: {e}"));
            Vec::new()
        }
    };
    let all = models.len() <= 40;
    let entries: Vec<Value> = models
        .iter()
        .map(|m| json!({"id": m["id"], "name": m["name"], "enabled": all, "contextLength": m["contextLength"],
                        "inputPricePerMtok": m["inputPricePerMtok"], "outputPricePerMtok": m["outputPricePerMtok"]}))
        .collect();
    match c.post("/api/providers", json!({"id": id, "baseUrl": url, "apiKey": key, "apiFormat": format, "models": entries})).await {
        Ok(_) => {
            ui::success(&format!("{id} verbunden · {} Modelle{}", entries.len(), if all { "" } else { " (im Web unter Modelle aktivieren)" }));
        }
        Err(e) => ui::error(&e.message),
    }
}

/* ================= main ================= */

fn help() {
    println!();
    for (c, d) in COMMANDS {
        println!("  {:<22} {}", c.with(ui::ACCENT), ui::dim(d));
    }
    println!();
    println!("  {}", ui::dim("Tipp: \\ am Zeilenende für mehrzeilige Eingabe · @datei fügt eine Datei ein"));
    println!();
}

/// Expands `@path` tokens into the file content (like other coding CLIs).
fn expand_mentions(input: &str, cwd: &std::path::Path) -> String {
    let mut extra = String::new();
    for tok in input.split_whitespace() {
        if let Some(p) = tok.strip_prefix('@') {
            let path = cwd.join(p);
            if path.is_file() {
                if let Ok(t) = std::fs::read_to_string(&path) {
                    let t: String = t.chars().take(60_000).collect();
                    extra.push_str(&format!("\n\n<file path=\"{p}\">\n{t}\n</file>"));
                }
            }
        }
    }
    format!("{input}{extra}")
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    {
        use std::io::IsTerminal;
        if !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some() {
            crossterm::style::force_color_output(false);
            ui::PLAIN.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
    let mut cfg = config::load();
    if args.logout {
        if let (Some(h), Some(s)) = (cfg.host.clone(), cfg.session.clone()) {
            let mut c = Client::new(&h);
            c.session = Some(s);
            let _ = c.logout().await;
        }
        cfg.session = None;
        config::save(&cfg);
        ui::success("Abgemeldet.");
        return;
    }
    let (client, user) = connect(&args, &mut cfg, false).await;
    let info = client.server_info().await.unwrap_or(Value::Null);
    let search = info["search"].as_bool().unwrap_or(false);
    let mode = Mode::parse(args.mode.as_deref().or(cfg.mode.as_deref()).unwrap_or("yolo"));
    let mut model = args.model.clone().or(cfg.model.clone()).unwrap_or_else(|| "multillm".into());
    // Validate the stored model still exists.
    let models = model_list(&client).await;
    if !models.iter().any(|m| m.id == model) {
        model = "multillm".into();
    }
    let mut agent = Agent::new(client, model, mode, search);

    if let Some(p) = &args.prompt {
        let input = expand_mentions(p, &agent.cwd);
        agent.turn(&input).await;
        return;
    }

    let uname = user["displayName"].as_str().or(user["username"].as_str()).unwrap_or("").to_string();
    println!();
    ui::banner(cfg.host.as_deref().unwrap_or(""), &uname, &short_model(&agent.model), agent.mode.as_str());
    if models.len() <= 1 {
        ui::info("Noch keine Modelle – verbinde mit /connect einen Provider.");
        println!();
    }

    let mut rl: Editor<ReplHelper, rustyline::history::DefaultHistory> = match Editor::new() {
        Ok(e) => e,
        Err(e) => {
            ui::error(&format!("Terminal nicht nutzbar: {e}"));
            return;
        }
    };
    rl.set_helper(Some(ReplHelper));
    let _ = std::fs::create_dir_all(config::dir());
    let _ = rl.load_history(&config::history_path());

    loop {
        let mode_tag = match agent.mode {
            Mode::Bypass => " bypass".with(ui::BAD).to_string(),
            Mode::Yolo => " yolo".with(ui::WARN).to_string(),
            Mode::Ask => String::new(),
        };
        println!("{}", ui::dim(&format!("─ {} ", short_model(&agent.model))) + &mode_tag);
        let prompt = format!("{} ", "❯".with(ui::ACCENT).bold());
        let mut input = match rl.readline(&prompt) {
            Ok(l) => l,
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(e) => {
                ui::error(&e.to_string());
                break;
            }
        };
        // Multi-line: trailing backslash continues.
        while input.ends_with('\\') {
            input.pop();
            input.push('\n');
            match rl.readline(&format!("{} ", "…".with(ui::DIM))) {
                Ok(l) => input.push_str(&l),
                Err(_) => break,
            }
        }
        let line = input.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(line.as_str());
        let _ = rl.save_history(&config::history_path());
        if line.starts_with('/') {
            let cmd = line.split_whitespace().next().unwrap_or("");
            let rest = line[cmd.len()..].trim();
            match cmd {
                "/help" | "/?" => help(),
                "/exit" | "/quit" | "/q" => break,
                "/model" | "/models" => {
                    if rest.is_empty() {
                        choose_model(&mut agent, &mut cfg).await;
                    } else {
                        agent.model = rest.to_string();
                        cfg.model = Some(agent.model.clone());
                        config::save(&cfg);
                        ui::success(&format!("Modell: {}", short_model(&agent.model)));
                    }
                }
                "/usage" | "/cost" => show_usage(&agent).await,
                "/connect" => connect_provider(&agent.client).await,
                "/yolo" => {
                    agent.mode = if agent.mode == Mode::Yolo { Mode::Ask } else { Mode::Yolo };
                    cfg.mode = Some(agent.mode.as_str().into());
                    config::save(&cfg);
                    if agent.mode == Mode::Yolo {
                        ui::info(&format!("Yolo-Modus {} – Befehle & Änderungen laufen ohne Nachfrage (gefährliche Befehle werden trotzdem bestätigt).", ui::warn("an")));
                    } else {
                        ui::info(&format!("Yolo-Modus {} – vor jeder Änderung wird gefragt.", ui::ok("aus")));
                    }
                }
                "/bypass-permissions" | "/bypass" => {
                    agent.mode = if agent.mode == Mode::Bypass { Mode::Yolo } else { Mode::Bypass };
                    cfg.mode = Some(agent.mode.as_str().into());
                    config::save(&cfg);
                    if agent.mode == Mode::Bypass {
                        ui::info(&format!("{} – ALLE Befehle werden ohne jede Nachfrage ausgeführt.", ui::bad("Bypass-Permissions an")));
                    } else {
                        ui::info("Bypass-Permissions aus (zurück im Yolo-Modus).");
                    }
                }
                "/ask" => {
                    agent.mode = Mode::Ask;
                    cfg.mode = Some("ask".into());
                    config::save(&cfg);
                    ui::info("Nachfragen vor jeder Änderung aktiviert.");
                }
                "/clear" | "/new" => {
                    agent.reset();
                    ui::success("Unterhaltung geleert.");
                }
                "/compact" => match agent.compact().await {
                    Ok(()) => ui::success("Unterhaltung zusammengefasst."),
                    Err(e) => ui::error(&e),
                },
                "/status" => {
                    println!("  {} {}", ui::dim("Host:   "), agent.client.base);
                    println!("  {} {}", ui::dim("User:   "), uname);
                    println!("  {} {}", ui::dim("Modell: "), short_model(&agent.model));
                    println!("  {} {}", ui::dim("Modus:  "), agent.mode.as_str());
                    println!("  {} {}", ui::dim("Suche:  "), if agent.search { "SearXNG" } else { "nicht verfügbar" });
                    println!("  {} {} in / {} out", ui::dim("Tokens: "), ui::fmt_num(agent.session_in as f64), ui::fmt_num(agent.session_out as f64));
                    println!("  {} {} Nachrichten", ui::dim("Verlauf:"), agent.messages.len().saturating_sub(1));
                }
                "/login" => {
                    let (c, _) = connect(&args, &mut cfg, true).await;
                    agent.client = c;
                }
                "/logout" => {
                    let _ = agent.client.logout().await;
                    cfg.session = None;
                    config::save(&cfg);
                    ui::success("Abgemeldet.");
                    break;
                }
                "/host" => {
                    cfg.host = None;
                    cfg.session = None;
                    let (c, _) = connect(&Args { host: None, prompt: None, logout: false, model: None, mode: None }, &mut cfg, true).await;
                    agent.client = c;
                    agent.model = "multillm".into();
                }
                other => ui::error(&format!("Unbekannter Befehl {other} – /help")),
            }
            continue;
        }
        let input = expand_mentions(&line, &agent.cwd);
        println!();
        agent.turn(&input).await;
    }
}
