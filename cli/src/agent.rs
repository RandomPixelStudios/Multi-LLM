//! The coding agent: conversation state, streaming completions, tool calls
//! and the permission layer (ask / yolo / bypass).

use crate::client::Client;
use crate::tools::{self, Kind};
use crate::ui::{self, Answer, Markdown, Spinner};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::PathBuf;

/* ---------- Ctrl+C ----------
 * One process-wide handler. While a task runs, Ctrl+C cancels it; otherwise
 * (prompts, password input, menus outside raw mode) it ends the program.
 * Without this, tokio keeps SIGINT captured after the first turn and Ctrl+C
 * silently did nothing anymore. */
pub static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static CANCEL: std::sync::OnceLock<tokio::sync::watch::Sender<u64>> = std::sync::OnceLock::new();

pub fn install_sigint() {
    let (tx, _) = tokio::sync::watch::channel(0u64);
    let _ = CANCEL.set(tx);
    tokio::spawn(async {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                return;
            }
            if BUSY.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some(tx) = CANCEL.get() {
                    tx.send_modify(|n| *n += 1);
                }
            } else {
                ui::restore_terminal();
                println!();
                std::process::exit(130);
            }
        }
    });
}

/// Cancellation for one busy section (task, OAuth wait). Dropping the guard
/// makes Ctrl+C quit again.
pub struct Busy {
    pub rx: tokio::sync::watch::Receiver<bool>,
    fwd: tokio::task::JoinHandle<()>,
}

impl Busy {
    pub fn start() -> Busy {
        BUSY.store(true, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = tokio::sync::watch::channel(false);
        let mut global = CANCEL.get().map(|c| c.subscribe());
        let fwd = tokio::spawn(async move {
            if let Some(g) = global.as_mut() {
                if g.changed().await.is_ok() {
                    let _ = tx.send(true);
                }
            }
        });
        Busy { rx, fwd }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.fwd.abort();
        BUSY.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Marker for errors worth retrying (stripped before display).
pub const RETRY: &str = "\u{1}retry:";

/// Network errors, rate limits and server errors are retryable; client
/// errors (bad request, unknown model, quota reached ...) are not.
pub fn retryable(e: crate::client::ApiError) -> String {
    let transient = e.status == 0
        || e.status == 408
        || e.status == 429 && e.code != "usage_limit_reached"
        || e.status >= 500;
    if transient { format!("{RETRY}{}", e.message) } else { e.message }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Confirm every write and command.
    Ask,
    /// Default: run everything, but confirm clearly dangerous commands and
    /// writes outside the working directory.
    Yolo,
    /// Never ask.
    Bypass,
}

impl Mode {
    pub fn parse(s: &str) -> Mode {
        match s {
            "ask" => Mode::Ask,
            "bypass" => Mode::Bypass,
            _ => Mode::Yolo,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Mode::Ask => "ask",
            Mode::Yolo => "yolo",
            Mode::Bypass => "bypass",
        }
    }
}

pub struct Agent {
    pub client: Client,
    pub model: String,
    pub mode: Mode,
    pub cwd: PathBuf,
    pub messages: Vec<Value>,
    pub search: bool,
    pub session_in: u64,
    pub session_out: u64,
    /// prompt tokens of the last request (= current context size)
    pub last_prompt_tokens: u64,
    /// context window of the selected model, if known
    pub context_len: Option<u64>,
    /// Swarm mode: the agent may split work into parallel sub-agents.
    pub swarm: bool,
    always: HashSet<String>,
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    args: String,
}

fn system_prompt(cwd: &std::path::Path, search: bool) -> String {
    let os = if cfg!(windows) { "Windows (PowerShell)" } else if cfg!(target_os = "macos") { "macOS (bash/zsh)" } else { "Linux (bash)" };
    let mut s = format!(
        "You are multillm, an expert software engineering agent running in the user's terminal.\n\
         Environment: {os}. Working directory: {}. Date: {}.\n\n\
         How to work:\n\
         - Use the tools to inspect the project before changing it. Read a file before editing it.\n\
         - Prefer edit_file for small changes and write_file for new files. Keep the existing style.\n\
         - Run builds/tests with bash to verify your changes when that is possible.\n\
         - Do what was asked - no unrelated refactors. Ask only when truly blocked.\n\
         - Be concise. Use Markdown sparingly; refer to code as path:line.\n\
         - Never print secrets. Never run destructive commands unless explicitly requested.\n{}",
        cwd.display(),
        chrono::Local::now().format("%Y-%m-%d"),
        if search { "- Use web_search / web_fetch for current information or documentation.\n" } else { "- Use web_fetch to read documentation URLs.\n" }
    );
    for name in ["MULTILLM.md", "AGENTS.md", "CLAUDE.md"] {
        if let Ok(t) = std::fs::read_to_string(cwd.join(name)) {
            let t: String = t.chars().take(20_000).collect();
            s.push_str(&format!("\nProject instructions ({name}):\n{t}\n"));
            break;
        }
    }
    s
}

impl Agent {
    pub fn new(client: Client, model: String, mode: Mode, search: bool) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut a = Self { client, model, mode, cwd, messages: Vec::new(), search, session_in: 0, session_out: 0, last_prompt_tokens: 0, context_len: None, swarm: false, always: HashSet::new() };
        a.reset();
        a
    }

    pub fn reset(&mut self) {
        self.messages = vec![json!({"role": "system", "content": system_prompt(&self.cwd, self.search)})];
    }

    /// One user turn incl. all tool round-trips. Ctrl+C cancels.
    pub async fn turn(&mut self, input: &str) {
        self.messages.push(json!({"role": "user", "content": input}));
        let busy = Busy::start();
        let cancel_rx = busy.rx.clone();
        let started = std::time::Instant::now();
        let mut steps = 0;
        loop {
            steps += 1;
            if steps > 60 {
                ui::error("Abgebrochen: zu viele Tool-Schritte in einer Runde.");
                break;
            }
            match self.complete(cancel_rx.clone()).await {
                Ok(Some(calls)) => {
                    if *cancel_rx.borrow() {
                        break;
                    }
                    for c in calls {
                        let out = self.execute(&c, &cancel_rx).await;
                        self.messages.push(json!({"role": "tool", "tool_call_id": c.id, "content": out}));
                        if *cancel_rx.borrow() {
                            break;
                        }
                    }
                    if *cancel_rx.borrow() {
                        ui::interrupted();
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    ui::error(&e);
                    // Drop a dangling user message so the history stays valid.
                    if self.messages.last().is_some_and(|m| m["role"] == "user") {
                        self.messages.pop();
                    }
                    break;
                }
            }
        }
        drop(busy);
        ui::turn_footer(started.elapsed().as_secs(), self.last_prompt_tokens, self.context_len);
    }

    /// Streams one completion. Returns tool calls (if any) after appending the
    /// assistant message to the history.
    /// One completion with automatic retries (empty answer, network error,
    /// interrupted stream, 429, 5xx). Backoff 2 s, 4 s, 8 s.
    async fn complete(&mut self, cancel: tokio::sync::watch::Receiver<bool>) -> Result<Option<Vec<Call>>, String> {
        const ATTEMPTS: u32 = 4;
        let mut attempt = 1;
        loop {
            match self.complete_once(cancel.clone()).await {
                Err(e) if e.starts_with(RETRY) && attempt < ATTEMPTS && !*cancel.borrow() => {
                    let msg = e.trim_start_matches(RETRY).to_string();
                    let wait = 2u64 << (attempt - 1);
                    ui::retry_notice(&msg, attempt + 1, ATTEMPTS, wait);
                    let mut c = cancel.clone();
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(wait)) => {}
                        _ = c.changed() => return Err("abgebrochen".into()),
                    }
                    attempt += 1;
                }
                Err(e) => {
                    let msg = e.trim_start_matches(RETRY).to_string();
                    return Err(if attempt > 1 { format!("{msg} (nach {attempt} Versuchen)") } else { msg });
                }
                ok => return ok,
            }
        }
    }

    async fn complete_once(&mut self, mut cancel: tokio::sync::watch::Receiver<bool>) -> Result<Option<Vec<Call>>, String> {
        let mut tool_defs = tools::definitions(self.search);
        if self.swarm {
            tool_defs.push(crate::swarm::tool_definition());
        }
        let body = json!({
            "model": self.model,
            "messages": self.messages,
            "tools": tool_defs,
            "stream": true,
            "stream_options": {"include_usage": true},
        });
        let mut spinner = Spinner::start("Arbeitet");
        let resp = tokio::select! {
            r = self.client.chat_stream(&body) => r,
            _ = cancel.changed() => { spinner.stop(); return Err("abgebrochen".into()); }
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) if e.status == 401 => {
                // API key rotated? Fetch the current one and retry once.
                spinner.stop();
                self.client.fetch_api_key().await.map_err(|e| format!("Anmeldung abgelaufen: {e} - /login"))?;
                spinner = Spinner::start("Arbeitet");
                self.client.chat_stream(&body).await.map_err(retryable)?
            }
            Err(e) => {
                spinner.stop();
                return Err(retryable(e));
            }
        };
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut reasoning_shown = false;
        let mut calls: Vec<Call> = Vec::new();
        let mut md = Markdown::default();
        let mut spinning = true;
        let mut usage: Option<Value> = None;
        let mut cancelled = false;
        'outer: loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = cancel.changed() => { cancelled = true; break 'outer; }
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| format!("{RETRY}Stream unterbrochen: {e}"))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(i) = buf.find('\n') {
                let line: String = buf.drain(..=i).collect();
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else { continue };
                let data = data.trim();
                if data == "[DONE]" {
                    break 'outer;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
                if let Some(err) = v.get("error") {
                    spinner.stop();
                    return Err(format!("{RETRY}{}", err["message"].as_str().unwrap_or("upstream error")));
                }
                if v["usage"].is_object() {
                    usage = Some(v["usage"].clone());
                }
                let delta = &v["choices"][0]["delta"];
                let r = delta["reasoning_content"].as_str().or(delta["reasoning"].as_str()).unwrap_or("");
                if !r.is_empty() && !reasoning_shown {
                    reasoning_shown = true;
                }
                if let Some(t) = delta["content"].as_str() {
                    if !t.is_empty() {
                        if spinning {
                            spinner.stop();
                            spinning = false;
                            md.start_message();
                        }
                        content.push_str(t);
                        md.push(t);
                    }
                }
                if let Some(tcs) = delta["tool_calls"].as_array() {
                    for tc in tcs {
                        let idx = tc["index"].as_u64().unwrap_or(calls.len() as u64) as usize;
                        while calls.len() <= idx {
                            calls.push(Call::default());
                        }
                        let c = &mut calls[idx];
                        if let Some(id) = tc["id"].as_str() {
                            if !id.is_empty() {
                                c.id = id.to_string();
                            }
                        }
                        if let Some(n) = tc["function"]["name"].as_str() {
                            c.name.push_str(n);
                        }
                        if let Some(a) = tc["function"]["arguments"].as_str() {
                            c.args.push_str(a);
                        }
                    }
                }
            }
        }
        spinner.stop();
        md.finish();
        if let Some(u) = usage {
            let i = u["prompt_tokens"].as_u64().unwrap_or(0);
            let o = u["completion_tokens"].as_u64().unwrap_or(0);
            self.session_in += i;
            self.session_out += o;
            self.last_prompt_tokens = i + o;
        }
        let calls: Vec<Call> = calls
            .into_iter()
            .filter(|c| !c.name.is_empty())
            .enumerate()
            .map(|(i, mut c)| {
                if c.id.is_empty() {
                    c.id = format!("call_{}_{}", self.messages.len(), i);
                }
                c
            })
            .collect();
        let mut msg = json!({"role": "assistant", "content": if content.is_empty() { Value::Null } else { json!(content) }});
        if !calls.is_empty() {
            msg["tool_calls"] = json!(calls
                .iter()
                .map(|c| json!({"id": c.id, "type": "function", "function": {"name": c.name, "arguments": if c.args.trim().is_empty() { "{}" } else { c.args.as_str() }}}))
                .collect::<Vec<_>>());
        }
        if content.is_empty() && calls.is_empty() {
            if cancelled {
                return Err("abgebrochen".into());
            }
            return Err(format!("{RETRY}Leere Antwort vom Modell."));
        }
        self.messages.push(msg);
        if cancelled {
            ui::interrupted();
            return Ok(None);
        }
        Ok(if calls.is_empty() { None } else { Some(calls) })
    }

    /// "Ran", "Read", "Edited" ... like Codex.
    fn title(name: &str, args: &Value) -> (String, String) {
        let s = |k: &str| args[k].as_str().unwrap_or("").to_string();
        let (verb, detail) = match name {
            "bash" => ("Bash", s("command")),
            "read_file" => ("Read", s("path")),
            "write_file" => ("Write", s("path")),
            "edit_file" => ("Update", s("path")),
            "glob" => ("Glob", s("pattern")),
            "grep" => ("Grep", s("pattern")),
            "web_fetch" => ("Fetch", s("url")),
            "web_search" => ("WebSearch", s("query")),
            other => (other, String::new()),
        };
        let mut d = detail.replace('\n', " ⏎ ");
        let max = ui::width().saturating_sub(verb.len() + 6);
        if d.chars().count() > max {
            d = d.chars().take(max.saturating_sub(1)).collect::<String>() + "…";
        }
        (verb.to_string(), d)
    }

    async fn execute(&mut self, c: &Call, cancel: &tokio::sync::watch::Receiver<bool>) -> String {
        let args: Value = serde_json::from_str(if c.args.trim().is_empty() { "{}" } else { &c.args }).unwrap_or_else(|_| json!({"_raw": c.args}));
        if args.get("_raw").is_some() {
            ui::tool_line(&c.name, "", false);
            ui::tool_out(&[ui::bad("ungültige Argumente")]);
            return "Error: tool arguments were not valid JSON. Retry with valid JSON.".into();
        }
        if c.name == "agent_swarm" {
            if !self.swarm {
                return "Error: swarm mode is off. Tell the user they can enable it with /swarm.".into();
            }
            let (out, _) = crate::swarm::run(&args, &self.client, &self.model, &self.cwd, self.search, self.mode == Mode::Bypass, cancel).await;
            return out;
        }
        let (verb, detail) = Self::title(&c.name, &args);
        if let Some(reason) = self.needs_permission(&c.name, &args) {
            ui::tool_line(&verb, &detail, true);
            match ui::approve(&reason) {
                Answer::Yes => {}
                Answer::Always => {
                    self.always.insert(self.always_key(&c.name, &args));
                }
                Answer::No => {
                    ui::tool_out(&[ui::warn("abgelehnt")]);
                    return "The user denied this action. Do not retry it; ask what to do instead or continue differently.".into();
                }
            }
        }
        // Snapshot for the diff of edits.
        let before = if c.name == "edit_file" || c.name == "write_file" {
            std::fs::read_to_string(self.cwd.join(args["path"].as_str().unwrap_or(""))).ok()
        } else {
            None
        };
        let (out, summary) = tools::run(&c.name, &args, &self.cwd, &self.client, cancel).await;
        let failed = out.starts_with("Error:");
        match c.name.as_str() {
            "edit_file" | "write_file" if !failed => {
                let after = std::fs::read_to_string(self.cwd.join(args["path"].as_str().unwrap_or(""))).unwrap_or_default();
                let path = args["path"].as_str().unwrap_or("");
                ui::diff_block(if before.is_some() { "Edited" } else { "Added" }, path, before.as_deref().unwrap_or(""), &after);
            }
            "bash" => {
                ui::tool_line("Bash", &detail, false);
                let lines: Vec<&str> = out.lines().filter(|l| !l.trim().is_empty() && !l.starts_with("[exit code")).collect();
                let mut show: Vec<String> = lines.iter().take(5).map(|l| ui::dim(&l.chars().take(ui::width().saturating_sub(8)).collect::<String>())).collect();
                if lines.len() > 5 {
                    show.push(ui::dim(&format!("… +{} Zeilen", lines.len() - 5)));
                }
                if failed {
                    show.push(ui::bad(&summary));
                } else if summary.contains("Exit") {
                    show.push(ui::bad(&summary));
                }
                ui::tool_out(&show);
            }
            _ => {
                ui::tool_line(&verb, &detail, false);
                ui::tool_out(&[if failed { ui::bad(&summary) } else { ui::dim(&summary) }]);
            }
        }
        out
    }

    fn always_key(&self, name: &str, args: &Value) -> String {
        match name {
            "bash" => format!("bash:{}", args["command"].as_str().unwrap_or("").split_whitespace().next().unwrap_or("")),
            other => other.to_string(),
        }
    }

    /// None = allowed without asking; Some(question) = ask the user.
    fn needs_permission(&self, name: &str, args: &Value) -> Option<String> {
        if self.mode == Mode::Bypass {
            return None;
        }
        if self.always.contains(&self.always_key(name, args)) {
            return None;
        }
        match tools::classify(name, args, &self.cwd) {
            Kind::ReadOnly => None,
            Kind::Command(cmd) => {
                if let Some(why) = tools::dangerous(&cmd) {
                    return Some(format!("Gefährlicher Befehl ({why}) ausführen?"));
                }
                if self.mode == Mode::Ask {
                    return Some("Befehl ausführen?".into());
                }
                None
            }
            Kind::Write(path) => {
                let inside = path.starts_with(&self.cwd);
                if !inside {
                    return Some(format!("Datei außerhalb des Projekts ändern ({})?", path.display()));
                }
                if self.mode == Mode::Ask {
                    return Some(format!("{} ändern?", path.strip_prefix(&self.cwd).unwrap_or(&path).display()));
                }
                None
            }
        }
    }

    /// Summarize the conversation into a single message (frees context).
    pub async fn compact(&mut self) -> Result<(), String> {
        if self.messages.len() < 3 {
            return Ok(());
        }
        let mut transcript = String::new();
        for m in self.messages.iter().skip(1) {
            let role = m["role"].as_str().unwrap_or("");
            let text = m["content"].as_str().unwrap_or("");
            let text: String = text.chars().take(4000).collect();
            transcript.push_str(&format!("[{role}] {text}\n"));
            if let Some(tc) = m["tool_calls"].as_array() {
                for c in tc {
                    transcript.push_str(&format!("[tool call] {} {}\n", c["function"]["name"], c["function"]["arguments"].as_str().unwrap_or("").chars().take(500).collect::<String>()));
                }
            }
        }
        let body = json!({"model": self.model, "stream": true, "messages": [
            {"role": "system", "content": "Summarize this coding session for continuation: goals, decisions, files changed, current state, open TODOs. Be dense and precise."},
            {"role": "user", "content": transcript}
        ]});
        let mut sp = Spinner::start("Fasse zusammen");
        let resp = self.client.chat_stream(&body).await.map_err(|e| e.message)?;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut summary = String::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            buf.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(i) = buf.find('\n') {
                let line: String = buf.drain(..=i).collect();
                if let Some(d) = line.trim().strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(d.trim()) {
                        if let Some(t) = v["choices"][0]["delta"]["content"].as_str() {
                            summary.push_str(t);
                        }
                    }
                }
            }
        }
        sp.stop();
        if summary.trim().is_empty() {
            return Err("Zusammenfassung leer".into());
        }
        self.reset();
        self.messages.push(json!({"role": "user", "content": format!("Summary of our previous conversation:\n{summary}")}));
        self.messages.push(json!({"role": "assistant", "content": "Understood - I have the context and will continue from here."}));
        Ok(())
    }
}
