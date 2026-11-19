//! Agent swarm (inspired by the Kimi Code CLI `/swarm`): the main agent splits
//! a task into independent subtasks, every subtask runs as its own sub-agent
//! (own context, own tool loop) in parallel, and the terminal shows a live
//! panel with one card per sub-agent: status spinner, type, title, elapsed
//! time, tool count and a two-row activity window.

use crate::client::Client;
use crate::tools::{self, Kind};
use crate::ui;
use crossterm::{
    cursor, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::io::{stdout, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

pub const MAX_PARALLEL: usize = 6;
const MAX_STEPS: usize = 30;

/// Tool definition offered to the main agent while swarm mode is on.
pub fn tool_definition() -> Value {
    json!({"type": "function", "function": {
        "name": "agent_swarm",
        "description": "Split the work into independent subtasks and run them in parallel as sub-agents. Each sub-agent has its own context and only sees its own prompt, so every prompt must be self-contained (goal, relevant files, constraints, expected result). Types: 'explore' = read-only research, 'plan' = analysis/design without shell, 'coder' = may edit files and run commands. Use 2-8 subtasks that do not edit the same files. Returns every sub-agent's final report.",
        "parameters": {"type": "object", "properties": {
            "tasks": {"type": "array", "items": {"type": "object", "properties": {
                "title": {"type": "string", "description": "short label (max 6 words)"},
                "type": {"type": "string", "enum": ["coder", "explore", "plan"]},
                "prompt": {"type": "string"}
            }, "required": ["title", "type", "prompt"]}}
        }, "required": ["tasks"]}
    }})
}

#[derive(Clone, Copy, PartialEq)]
enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

struct Card {
    title: String,
    kind: String,
    status: Status,
    started: Option<Instant>,
    finished: Option<Instant>,
    tools: usize,
    tokens: u64,
    activity: Vec<String>,
    result: String,
}

type Cards = Arc<Mutex<Vec<Card>>>;

fn push_activity(cards: &Cards, i: usize, line: String) {
    let mut c = cards.lock().unwrap();
    let a = &mut c[i].activity;
    a.push(line);
    if a.len() > 2 {
        a.remove(0);
    }
}

/// Runs the swarm and returns the combined report for the main agent.
pub async fn run(
    args: &Value,
    client: &Client,
    model: &str,
    cwd: &PathBuf,
    search: bool,
    bypass: bool,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> (String, String) {
    let tasks: Vec<(String, String, String)> = args["tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|t| {
            let p = t["prompt"].as_str()?.to_string();
            let k = match t["type"].as_str().unwrap_or("coder") {
                "explore" => "explore",
                "plan" => "plan",
                _ => "coder",
            };
            Some((t["title"].as_str().unwrap_or("Teilaufgabe").to_string(), k.to_string(), p))
        })
        .take(12)
        .collect();
    if tasks.is_empty() {
        return ("Error: agent_swarm needs a non-empty 'tasks' list.".into(), "keine Teilaufgaben".into());
    }
    let cards: Cards = Arc::new(Mutex::new(
        tasks
            .iter()
            .map(|(t, k, _)| Card {
                title: t.clone(),
                kind: k.clone(),
                status: Status::Queued,
                started: None,
                finished: None,
                tools: 0,
                tokens: 0,
                activity: vec![],
                result: String::new(),
            })
            .collect(),
    ));
    let started = Instant::now();
    let sem = Arc::new(tokio::sync::Semaphore::new(MAX_PARALLEL));
    let mut set = tokio::task::JoinSet::new();
    for (i, (_, kind, prompt)) in tasks.iter().cloned().enumerate() {
        let (cards, client, model, cwd, sem, cancel) = (cards.clone(), client.clone(), model.to_string(), cwd.clone(), sem.clone(), cancel.clone());
        set.spawn(async move {
            let _permit = sem.acquire_owned().await;
            if *cancel.borrow() {
                cards.lock().unwrap()[i].status = Status::Cancelled;
                return;
            }
            {
                let mut c = cards.lock().unwrap();
                c[i].status = Status::Running;
                c[i].started = Some(Instant::now());
            }
            let r = sub_agent(i, &kind, &prompt, &client, &model, &cwd, search, bypass, &cards, cancel.clone()).await;
            let mut c = cards.lock().unwrap();
            c[i].finished = Some(Instant::now());
            match r {
                Ok(text) => {
                    c[i].status = Status::Done;
                    c[i].result = text;
                }
                Err(e) => {
                    c[i].status = if *cancel.borrow() { Status::Cancelled } else { Status::Failed };
                    c[i].result = format!("Error: {e}");
                }
            }
        });
    }

    // Live panel.
    let mut panel = Panel { drawn: 0 };
    let mut tick = 0usize;
    loop {
        panel.draw(&cards, started, tick, false);
        tick += 1;
        tokio::select! {
            r = set.join_next() => if r.is_none() { break },
            _ = tokio::time::sleep(Duration::from_millis(110)) => {}
        }
    }
    panel.draw(&cards, started, tick, true);

    let c = cards.lock().unwrap();
    let done = c.iter().filter(|c| c.status == Status::Done).count();
    let mut report = String::new();
    for (i, card) in c.iter().enumerate() {
        report.push_str(&format!("## Sub-agent {} ({}): {}\n{}\n\n", i + 1, card.kind, card.title, card.result.trim()));
    }
    (report, format!("{done}/{} fertig", c.len()))
}

struct Panel {
    drawn: u16,
}

impl Panel {
    fn draw(&mut self, cards: &Cards, started: Instant, tick: usize, final_: bool) {
        let c = cards.lock().unwrap();
        let w = ui::width();
        let h = terminal::size().map(|(_, h)| h as usize).unwrap_or(40);
        // Cards keep a stable height (3 rows); fall back to 1 row when many.
        let compact = c.len() * 3 + 3 > h.saturating_sub(4);
        let mut out = stdout();
        if self.drawn > 0 {
            let _ = queue!(out, cursor::MoveUp(self.drawn));
        }
        let _ = queue!(out, Print("\r"), terminal::Clear(ClearType::FromCursorDown));
        let mut lines = 0u16;
        let running = c.iter().filter(|c| c.status == Status::Running).count();
        let done = c.iter().filter(|c| matches!(c.status, Status::Done)).count();
        let failed = c.iter().filter(|c| matches!(c.status, Status::Failed | Status::Cancelled)).count();
        let head = format!(
            "{} {}{}{}",
            if final_ { "⏺".with_c(ui::OK) } else { "⏺".with_c(ui::STAR) },
            ui::bold("Swarm"),
            ui::dim("("),
            ui::dim(&format!(
                "{} Agenten · {} aktiv · {} fertig{} · {}s)",
                c.len(),
                running,
                done,
                if failed > 0 { format!(" · {failed} fehlgeschlagen") } else { String::new() },
                started.elapsed().as_secs()
            ))
        );
        let _ = queue!(out, Print(head), Print("\r\n"));
        lines += 1;
        let spin = ["◐", "◓", "◑", "◒"];
        for (i, card) in c.iter().enumerate() {
            let (sym, col) = match card.status {
                Status::Queued => ("○".to_string(), ui::DIM),
                Status::Running => (spin[(tick / 2 + i) % 4].to_string(), kind_color(&card.kind)),
                Status::Done => ("✓".to_string(), ui::OK),
                Status::Failed => ("✗".to_string(), ui::BAD),
                Status::Cancelled => ("■".to_string(), ui::WARN),
            };
            let secs = match (card.started, card.finished) {
                (Some(s), Some(f)) => (f - s).as_secs(),
                (Some(s), None) => s.elapsed().as_secs(),
                _ => 0,
            };
            let right = match card.status {
                Status::Queued => "wartet".to_string(),
                _ => format!("{secs}s · {} Tools · {} Tok", card.tools, ui::fmt_num(card.tokens as f64)),
            };
            let tag = format!("{:<7}", card.kind);
            let room = w.saturating_sub(right.width() + tag.width() + 10);
            let title: String = if card.title.width() > room { card.title.chars().take(room.saturating_sub(1)).collect::<String>() + "…" } else { card.title.clone() };
            let gap = w.saturating_sub(4 + tag.width() + 1 + title.width() + right.width() + 2);
            let _ = queue!(
                out,
                Print("  "),
                SetForegroundColor(col),
                Print(&sym),
                Print(" "),
                SetForegroundColor(kind_color(&card.kind)),
                Print(&tag),
                ResetColor,
                Print(" "),
                Print(if card.status == Status::Queued { ui::dim(&title) } else { ui::bold(&title) }),
                Print(" ".repeat(gap)),
                SetForegroundColor(ui::DIM),
                Print(&right),
                ResetColor,
                Print("\r\n")
            );
            lines += 1;
            if compact {
                continue;
            }
            // Two-row activity window (stable height).
            let act: Vec<String> = match card.status {
                Status::Done | Status::Failed | Status::Cancelled if final_ => {
                    let first = card.result.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim_start_matches('#').trim().to_string();
                    vec![first, String::new()]
                }
                _ => {
                    let mut a = card.activity.clone();
                    while a.len() < 2 {
                        a.push(String::new());
                    }
                    a
                }
            };
            for (k, a) in act.iter().take(2).enumerate() {
                let a: String = a.chars().take(w.saturating_sub(10)).collect();
                let lead = if k == 0 { "⎿" } else { " " };
                let _ = queue!(out, Print("     "), SetForegroundColor(ui::DIM), Print(lead), Print("  "), Print(&a), ResetColor, Print("\r\n"));
                lines += 1;
            }
        }
        if final_ {
            let _ = queue!(out, Print("\r\n"));
            lines = 0; // keep the final panel on screen
        }
        let _ = out.flush();
        self.drawn = lines;
    }
}

trait WithC {
    fn with_c(&self, c: Color) -> String;
}
impl WithC for &str {
    fn with_c(&self, c: Color) -> String {
        use crossterm::style::Stylize;
        if ui::PLAIN.load(std::sync::atomic::Ordering::Relaxed) {
            self.to_string()
        } else {
            self.with(c).to_string()
        }
    }
}

fn kind_color(kind: &str) -> Color {
    match kind {
        "explore" => ui::TEAL,
        "plan" => ui::STAR,
        _ => ui::ACCENT,
    }
}

fn sub_system_prompt(kind: &str, cwd: &std::path::Path) -> String {
    let role = match kind {
        "explore" => "You are a read-only research sub-agent. Investigate and report findings precisely (files, lines, facts). Never modify anything.",
        "plan" => "You are a planning sub-agent. Read what you need and produce a concrete, step-by-step plan. Do not modify files or run commands.",
        _ => "You are a coding sub-agent. Implement exactly your subtask, verify it if possible, and stay out of files that other subtasks are likely to edit.",
    };
    format!(
        "{role}\nWorking directory: {}. OS: {}.\nYou run autonomously in parallel with other sub-agents; nobody can answer questions. \
         Finish with a concise report: what you did or found, changed files, open issues.",
        cwd.display(),
        std::env::consts::OS
    )
}

fn tool_label(name: &str, args: &Value) -> String {
    let s = |k: &str| args[k].as_str().unwrap_or("").replace('\n', " ");
    let (v, d) = match name {
        "bash" => ("Bash", s("command")),
        "read_file" => ("Read", s("path")),
        "write_file" => ("Write", s("path")),
        "edit_file" => ("Update", s("path")),
        "glob" => ("Glob", s("pattern")),
        "grep" => ("Grep", s("pattern")),
        "web_fetch" => ("Fetch", s("url")),
        "web_search" => ("WebSearch", s("query")),
        o => (o, String::new()),
    };
    format!("{v}({d})")
}

#[allow(clippy::too_many_arguments)]
async fn sub_agent(
    i: usize,
    kind: &str,
    prompt: &str,
    client: &Client,
    model: &str,
    cwd: &PathBuf,
    search: bool,
    bypass: bool,
    cards: &Cards,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<String, String> {
    let read_only = kind != "coder";
    let defs: Vec<Value> = tools::definitions(search)
        .into_iter()
        .filter(|d| {
            let n = d["function"]["name"].as_str().unwrap_or("");
            !read_only || !matches!(n, "bash" | "write_file" | "edit_file")
        })
        .collect();
    let mut messages = vec![
        json!({"role": "system", "content": sub_system_prompt(kind, cwd)}),
        json!({"role": "user", "content": prompt}),
    ];
    for _ in 0..MAX_STEPS {
        let body = json!({"model": model, "messages": messages, "tools": defs, "stream": true, "stream_options": {"include_usage": true}});
        let resp = tokio::select! {
            r = client.chat_stream(&body) => r.map_err(|e| e.message)?,
            _ = cancel.changed() => return Err("abgebrochen".into()),
        };
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut calls: Vec<(String, String, String)> = Vec::new();
        'read: loop {
            let chunk = tokio::select! {
                c = stream.next() => c,
                _ = cancel.changed() => return Err("abgebrochen".into()),
            };
            let Some(chunk) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&chunk.map_err(|e| e.to_string())?));
            while let Some(p) = buf.find('\n') {
                let line: String = buf.drain(..=p).collect();
                let Some(d) = line.trim().strip_prefix("data:") else { continue };
                if d.trim() == "[DONE]" {
                    break 'read;
                }
                let Ok(v) = serde_json::from_str::<Value>(d.trim()) else { continue };
                if let Some(e) = v.get("error") {
                    return Err(e["message"].as_str().unwrap_or("upstream error").to_string());
                }
                if let Some(u) = v["usage"].as_object() {
                    let t = u.get("total_tokens").and_then(Value::as_u64).unwrap_or(0);
                    cards.lock().unwrap()[i].tokens += t;
                }
                let delta = &v["choices"][0]["delta"];
                if let Some(t) = delta["content"].as_str() {
                    content.push_str(t);
                    if let Some(last) = content.lines().last() {
                        let mut c = cards.lock().unwrap();
                        let a = &mut c[i].activity;
                        let line = format!("💬 {}", last.trim());
                        if a.last().is_some_and(|l| l.starts_with("💬")) {
                            *a.last_mut().unwrap() = line;
                        } else {
                            a.push(line);
                            if a.len() > 2 {
                                a.remove(0);
                            }
                        }
                    }
                }
                for tc in delta["tool_calls"].as_array().cloned().unwrap_or_default() {
                    let idx = tc["index"].as_u64().unwrap_or(calls.len() as u64) as usize;
                    while calls.len() <= idx {
                        calls.push(Default::default());
                    }
                    if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                        calls[idx].0 = id.to_string();
                    }
                    if let Some(n) = tc["function"]["name"].as_str() {
                        calls[idx].1.push_str(n);
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str() {
                        calls[idx].2.push_str(a);
                    }
                }
            }
        }
        let calls: Vec<(String, String, String)> = calls
            .into_iter()
            .filter(|c| !c.1.is_empty())
            .enumerate()
            .map(|(k, mut c)| {
                if c.0.is_empty() {
                    c.0 = format!("s{i}_{}_{k}", messages.len());
                }
                c
            })
            .collect();
        let mut msg = json!({"role": "assistant", "content": if content.is_empty() { Value::Null } else { json!(content) }});
        if calls.is_empty() {
            return Ok(if content.trim().is_empty() { "(no report)".into() } else { content });
        }
        msg["tool_calls"] = json!(calls.iter().map(|(id, n, a)| json!({"id": id, "type": "function", "function": {"name": n, "arguments": if a.trim().is_empty() { "{}" } else { a.as_str() }}})).collect::<Vec<_>>());
        messages.push(msg);
        for (id, name, a) in calls {
            let args: Value = serde_json::from_str(if a.trim().is_empty() { "{}" } else { &a }).unwrap_or(json!({}));
            push_activity(cards, i, format!("⏺ {}", tool_label(&name, &args)));
            cards.lock().unwrap()[i].tools += 1;
            // Sub-agents cannot ask: read-only types never write, and clearly
            // dangerous commands are refused unless bypass-permissions is on.
            let denied = match tools::classify(&name, &args, cwd) {
                Kind::Command(_) | Kind::Write(_) if read_only => Some("this sub-agent type is read-only"),
                Kind::Command(cmd) if !bypass && tools::dangerous(&cmd).is_some() => Some("dangerous command refused in swarm mode"),
                Kind::Write(p) if !bypass && !p.starts_with(cwd) => Some("writing outside the working directory is not allowed in swarm mode"),
                _ => None,
            };
            let out = match denied {
                Some(why) => format!("Error: {why}"),
                None => tools::run(&name, &args, cwd, client, &cancel).await.0,
            };
            messages.push(json!({"role": "tool", "tool_call_id": id, "content": out}));
            if *cancel.borrow() {
                return Err("abgebrochen".into());
            }
        }
    }
    Err(format!("stopped after {MAX_STEPS} steps"))
}
