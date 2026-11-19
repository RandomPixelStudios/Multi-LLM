//! Terminal rendering: colors, spinner, list picker, confirm prompts and a
//! small streaming Markdown renderer.

use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor, Stylize},
    terminal::{self, ClearType},
};
use std::io::{stdout, IsTerminal, Write};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use unicode_width::UnicodeWidthStr;

pub const ACCENT: Color = Color::Rgb { r: 124, g: 140, b: 255 };
pub const TEAL: Color = Color::Rgb { r: 56, g: 213, b: 200 };
pub const DIM: Color = Color::Rgb { r: 120, g: 126, b: 142 };
pub const OK: Color = Color::Rgb { r: 34, g: 197, b: 94 };
pub const WARN: Color = Color::Rgb { r: 245, g: 158, b: 11 };
pub const BAD: Color = Color::Rgb { r: 239, g: 68, b: 68 };
/// Warm accent used for the welcome box and the working star.
pub const STAR: Color = Color::Rgb { r: 215, g: 119, b: 87 };

pub fn width() -> usize {
    terminal::size().map(|(w, _)| w as usize).unwrap_or(100).clamp(40, 200)
}

/// Colors off (pipe / NO_COLOR): crossterm still emits reset codes, so the
/// helpers return plain text instead.
pub static PLAIN: AtomicBool = AtomicBool::new(false);
fn plain() -> bool {
    PLAIN.load(Ordering::Relaxed)
}

pub fn dim(s: &str) -> String {
    if plain() { return s.to_string(); }
    s.with(DIM).to_string()
}
pub fn bold(s: &str) -> String {
    if plain() { return s.to_string(); }
    s.bold().to_string()
}
pub fn ok(s: &str) -> String {
    if plain() { return s.to_string(); }
    s.with(OK).to_string()
}
pub fn warn(s: &str) -> String {
    if plain() { return s.to_string(); }
    s.with(WARN).to_string()
}
pub fn bad(s: &str) -> String {
    if plain() { return s.to_string(); }
    s.with(BAD).to_string()
}

pub fn info(msg: &str) {
    println!("{} {}", "●".with(TEAL), msg);
}
pub fn error(msg: &str) {
    println!("{} {}", "✗".with(BAD), msg);
}
pub fn success(msg: &str) {
    println!("{} {}", "✓".with(OK), msg);
}

/// Codex-style session box:
/// ╭──────────────────────────────────────╮
/// │ >_ Multi LLM (v2.0.0)                │
/// │                                      │
/// │ model:     gpt-x      /model to change│
/// │ directory: ~/project                 │
/// ╰──────────────────────────────────────╯
pub fn banner(host: &str, user: &str, model: &str, mode: &str, cwd: &str) {
    let rows: Vec<(String, usize)> = {
        let mut v = Vec::new();
        let title = format!("✻ Willkommen bei Multi LLM  v{}", env!("CARGO_PKG_VERSION"));
        v.push((if plain() { title.clone() } else { format!("{} {}{}", "✻".with(STAR), "Willkommen bei Multi LLM".bold(), dim(&format!("  v{}", env!("CARGO_PKG_VERSION")))) }, title.width()));
        v.push((String::new(), 0));
        let kv = |k: &str, val: &str, hint: &str| -> (String, usize) {
            // Keep the box inside the terminal: shorten long values from the left.
            let room = width().saturating_sub(16 + hint.width() + if hint.is_empty() { 0 } else { 3 }).max(10);
            let owned;
            let val = if val.width() > room {
                let tail: String = val.chars().rev().take(room - 1).collect::<Vec<_>>().into_iter().rev().collect();
                owned = format!("…{tail}");
                owned.as_str()
            } else {
                val
            };
            let plain_s = format!("{k:<11}{val}{}", if hint.is_empty() { String::new() } else { format!("   {hint}") });
            let styled = format!("{}{}{}", dim(&format!("{k:<11}")), val, if hint.is_empty() { String::new() } else { format!("   {}", dim(hint)) });
            (styled, plain_s.width())
        };
        v.push(kv("model:", model, "/model zum Wechseln"));
        v.push(kv("directory:", cwd, ""));
        v.push(kv("server:", &format!("{host} · {user}"), ""));
        let m = match mode {
            "bypass" => "bypass-permissions (keine Nachfragen)",
            "yolo" => "yolo (nur gefährliche Befehle bestätigen)",
            _ => "ask (vor jeder Änderung fragen)",
        };
        v.push(kv("approval:", m, ""));
        v
    };
    let inner = rows.iter().map(|r| r.1).max().unwrap_or(10).min(width().saturating_sub(4)) + 2;
    let line = "─".repeat(inner);
    let edge = |t: &str| if plain() { t.to_string() } else { t.with(STAR).to_string() };
    println!("{}", edge(&format!("╭{line}╮")));
    for (txt, w) in rows {
        let pad = inner.saturating_sub(w + 1);
        println!("{} {}{}{}", edge("│"), txt, " ".repeat(pad), edge("│"));
    }
    println!("{}", edge(&format!("╰{line}╯")));
    println!();
    println!("  {}", dim("Beschreibe eine Aufgabe oder nutze einen Befehl:"));
    for (c, d) in [("/model", "Modell wählen"), ("/connect", "Provider hinzufügen"), ("/usage", "Nutzung & Limits"), ("/help", "alle Befehle")] {
        println!("  {} {}", c.with(ACCENT), dim(&format!("- {d}")));
    }
    println!();
}

/// Turns raw upstream errors (OpenRouter JSON etc.) into one readable line:
/// "AtlasCloud (über OpenRouter) lehnte die Anfrage ab: 400 bad request".
pub fn pretty_error(raw: &str) -> String {
    let Some(start) = raw.find('{') else { return raw.to_string() };
    let json_part = &raw[start..];
    // The server may cut the snippet; try progressively shorter prefixes.
    let v: Option<serde_json::Value> = serde_json::from_str(json_part).ok().or_else(|| {
        (1..json_part.len()).rev().filter(|i| json_part.is_char_boundary(*i)).take(400).find_map(|i| {
            serde_json::from_str::<serde_json::Value>(&format!("{}}}}}", &json_part[..i])).ok()
        })
    });
    let Some(v) = v else {
        // Fallback: pull out the interesting bits with plain text search.
        let grab = |key: &str| -> Option<String> {
            let k = format!("\"{key}\":\"");
            let i = raw.find(&k)? + k.len();
            Some(raw[i..].split('"').next()?.to_string())
        };
        if let Some(p) = grab("provider_name") {
            return format!("{p} lehnte die Anfrage ab ({})", grab("message").unwrap_or_else(|| "bad request".into()));
        }
        return raw.chars().take(200).collect();
    };
    let e = &v["error"];
    let code = e["code"].as_u64().map(|c| c.to_string()).unwrap_or_default();
    let mut msg = e["message"].as_str().unwrap_or("Fehler").to_string();
    if let Some(raw_inner) = e["metadata"]["raw"].as_str() {
        if let Ok(inner) = serde_json::from_str::<serde_json::Value>(raw_inner) {
            if let Some(m) = inner["msg"].as_str().or(inner["message"].as_str()).or(inner["error"]["message"].as_str()) {
                msg = m.to_string();
            }
        }
    }
    match e["metadata"]["provider_name"].as_str() {
        Some(p) => format!("{p} (über OpenRouter) lehnte die Anfrage ab: {code} {msg}"),
        None => format!("Provider lehnte die Anfrage ab: {code} {msg}"),
    }
}

/// "⎿ Leere Antwort – neuer Versuch 2/4 in 2 s"
pub fn retry_notice(msg: &str, attempt: u32, max: u32, wait: u64) {
    let room = width().saturating_sub(34).max(20);
    let first = msg.lines().next().unwrap_or("");
    let short: String = if first.chars().count() > room { first.chars().take(room - 1).collect::<String>() + "…" } else { first.to_string() };
    let msg = short.as_str();
    let when = if wait == 0 { "ohne Zusatzfelder".to_string() } else { format!("in {wait}s") };
    let n = if attempt == max && wait == 0 { String::new() } else { format!("{attempt}/{max} ") };
    println!("  {}  {} {}", dim("⎿"), warn(msg), dim(&format!("· neuer Versuch {n}{when}")));
}

/// Put the terminal back into a sane state (raw mode off, cursor visible).
pub fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(stdout(), cursor::Show, ResetColor);
}

/// "■ Unterbrochen" after Ctrl+C.
pub fn interrupted() {
    println!("  {}  {}", dim("⎿"), bad("Unterbrochen · was soll ich stattdessen tun?"));
}

/// Codex-like footer after a turn: elapsed time and context left.
pub fn turn_footer(secs: u64, _ctx_used: u64, _ctx_len: Option<u64>) {
    // Context usage lives in the input box status line; only long turns
    // get a short timing note.
    if secs >= 10 {
        println!("{}", dim(&format!("  ✻ {secs}s")));
    }
    println!();
}

/// "⏺ Bash(cargo test)" like Claude Code. `pending` = waiting for approval.
pub fn tool_line(verb: &str, detail: &str, pending: bool) {
    let dot = if plain() { "⏺".to_string() } else if pending { "⏺".with(WARN).to_string() } else { "⏺".with(OK).to_string() };
    if detail.is_empty() {
        println!("{} {}", dot, bold(verb));
    } else {
        println!("{} {}{}{}{}", dot, bold(verb), dim("("), detail, dim(")"));
    }
}

/// "  ⎿  first line" + aligned following lines.
pub fn tool_out(lines: &[String]) {
    for (i, l) in lines.iter().enumerate() {
        println!("  {}  {}", if i == 0 { dim("⎿") } else { " ".to_string() }, l);
    }
}

/// "⏺ Update(src/x.rs)" + summary + diff with colored backgrounds.
pub fn diff_block(verb: &str, path: &str, before: &str, after: &str) {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let mut pre = 0;
    while pre < old.len() && pre < new.len() && old[pre] == new[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < old.len() - pre && suf < new.len() - pre && old[old.len() - 1 - suf] == new[new.len() - 1 - suf] {
        suf += 1;
    }
    let removed = &old[pre..old.len() - suf];
    let added = &new[pre..new.len() - suf];
    tool_line(if verb == "Added" { "Write" } else { "Update" }, path, false);
    let plural = |n: usize, one: &str, many: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {many}") };
    let summary = if verb == "Added" {
        format!("{} geschrieben", plural(added.len(), "Zeile", "Zeilen"))
    } else {
        format!("{} hinzugefügt, {} entfernt", plural(added.len(), "Zeile", "Zeilen"), plural(removed.len(), "Zeile", "Zeilen"))
    };
    tool_out(&[dim(&summary)]);
    let w = width().saturating_sub(8);
    let row = |num: usize, sign: char, text: &str, bg: Option<Color>| {
        let body: String = format!("{:>4} {} {}", num, sign, text).chars().take(w).collect();
        let pad = w.saturating_sub(body.width());
        if plain() || bg.is_none() {
            println!("     {}", if bg.is_none() { dim(&body) } else { body });
        } else {
            let mut out = stdout();
            let _ = queue!(
                out,
                Print("     "),
                crossterm::style::SetBackgroundColor(bg.unwrap()),
                SetForegroundColor(Color::Rgb { r: 230, g: 232, b: 238 }),
                Print(&body),
                Print(" ".repeat(pad)),
                ResetColor,
                Print("\n")
            );
            let _ = out.flush();
        }
    };
    let limit = 16;
    let mut shown = 0;
    if pre > 0 && pre <= old.len() {
        row(pre, ' ', old[pre - 1], None);
    }
    for (i, l) in removed.iter().enumerate() {
        if shown >= limit { break; }
        row(pre + i + 1, '-', l, Some(Color::Rgb { r: 92, g: 30, b: 36 }));
        shown += 1;
    }
    for (i, l) in added.iter().enumerate() {
        if shown >= limit { break; }
        row(pre + i + 1, '+', l, Some(Color::Rgb { r: 26, g: 74, b: 42 }));
        shown += 1;
    }
    if new.len() > pre + added.len() && suf > 0 {
        let idx = pre + added.len();
        row(idx + 1, ' ', new[idx], None);
    }
    let total = removed.len() + added.len();
    if total > shown {
        println!("     {}", dim(&format!("… +{} weitere Zeilen", total - shown)));
    }
}

/// Codex-style approval menu.
pub fn approve(question: &str) -> Answer {
    println!("  {}", bold(question));
    if !std::io::stdin().is_terminal() {
        return Answer::No;
    }
    let items = vec![
        Item { label: "Ja, ausführen".into(), detail: "j".into() },
        Item { label: "Ja, und für diese Sitzung nicht mehr fragen".into(), detail: "i".into() },
        Item { label: "Nein, sag dem Agenten, was er anders machen soll".into(), detail: "n / Esc".into() },
    ];
    match pick("", &items, 0) {
        Some(0) => Answer::Yes,
        Some(1) => Answer::Always,
        _ => Answer::No,
    }
}

/* ---------------- spinner ---------------- */

pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub fn start(label: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let label = label.to_string();
        let tty = stdout().is_terminal();
        let handle = std::thread::spawn(move || {
            if !tty {
                return;
            }
            let started = std::time::Instant::now();
            let glyphs = ["·", "✢", "✳", "✶", "✻", "✽", "✻", "✶", "✳", "✢"];
            let verbs = ["Denkt nach", "Tüftelt", "Grübelt", "Werkelt", "Kombiniert", "Sinniert", "Baut"];
            let custom = label != "Arbeitet";
            let mut i = 0usize;
            while !s2.load(Ordering::Relaxed) {
                let secs = started.elapsed().as_secs();
                let verb = if custom { label.as_str() } else { verbs[(secs as usize / 4) % verbs.len()] };
                let text = format!("{verb}…");
                let chars: Vec<char> = text.chars().collect();
                let hot = (i / 2) % (chars.len() + 8);
                let mut out = stdout();
                let _ = queue!(out, Print("\r"), SetForegroundColor(STAR), Print(glyphs[(i / 2) % glyphs.len()]), Print(" "));
                for (k, ch) in chars.iter().enumerate() {
                    let c = if k + 1 >= hot && k <= hot + 1 { Color::Rgb { r: 255, g: 190, b: 160 } } else { STAR };
                    let _ = queue!(out, SetForegroundColor(c), Print(ch));
                }
                let _ = queue!(
                    out,
                    SetForegroundColor(DIM),
                    Print(format!(" ({secs}s · Ctrl+C zum Unterbrechen)")),
                    ResetColor,
                    terminal::Clear(ClearType::UntilNewLine)
                );
                let _ = out.flush();
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(90));
            }
            let mut out = stdout();
            let _ = queue!(out, Print("\r"), terminal::Clear(ClearType::CurrentLine));
            let _ = out.flush();
        });
        Self { stop, handle: Some(handle) }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

/* ---------------- picker ---------------- */

pub struct Item {
    pub label: String,
    pub detail: String,
}

/// Arrow-key list picker with type-to-filter. Returns the index into `items`.
pub fn pick(title: &str, items: &[Item], initial: usize) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    if !stdout().is_terminal() {
        return numbered_pick(title, items);
    }
    let mut filter = String::new();
    let mut sel = initial.min(items.len() - 1);
    let max_rows = (terminal::size().map(|(_, h)| h as usize).unwrap_or(24)).saturating_sub(6).clamp(5, 18);
    let mut out = stdout();
    if terminal::enable_raw_mode().is_err() {
        return numbered_pick(title, items);
    }
    let _ = execute!(out, cursor::Hide);
    let mut drawn = 0usize;
    let result = loop {
        let visible: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                filter.is_empty()
                    || it.label.to_lowercase().contains(&filter.to_lowercase())
                    || it.detail.to_lowercase().contains(&filter.to_lowercase())
            })
            .map(|(i, _)| i)
            .collect();
        let pos = visible.iter().position(|&i| i == sel).unwrap_or(0);
        if !visible.is_empty() {
            sel = visible[pos];
        }
        let start = pos.saturating_sub(max_rows.saturating_sub(1));
        // redraw
        if drawn > 0 {
            let _ = queue!(out, cursor::MoveUp(drawn as u16));
        }
        let _ = queue!(out, Print("\r"), terminal::Clear(ClearType::FromCursorDown));
        let _ = queue!(
            out,
            Print(if title.is_empty() { "  " } else { "" }),
            SetAttribute(Attribute::Bold),
            Print(title),
            SetAttribute(Attribute::Reset),
            SetForegroundColor(DIM),
            Print(if filter.is_empty() { "  (↑↓ Enter · tippen zum Filtern · Esc)".to_string() } else { format!("  Filter: {filter}") }),
            ResetColor,
            Print("\r\n")
        );
        let mut lines = 1;
        let w = width();
        for &i in visible.iter().skip(start).take(max_rows) {
            let it = &items[i];
            let cur = i == sel;
            let mut label = it.label.clone();
            let maxl = w.saturating_sub(it.detail.width() + 8).max(10);
            if label.width() > maxl {
                label = label.chars().take(maxl.saturating_sub(1)).collect::<String>() + "…";
            }
            if cur {
                let _ = queue!(out, SetForegroundColor(ACCENT), Print(" ❯ "), SetAttribute(Attribute::Bold), Print(&label), SetAttribute(Attribute::Reset));
            } else {
                let _ = queue!(out, Print("   "), Print(&label));
            }
            let _ = queue!(out, SetForegroundColor(DIM), Print(format!("  {}", it.detail)), ResetColor, Print("\r\n"));
            lines += 1;
        }
        if visible.is_empty() {
            let _ = queue!(out, SetForegroundColor(DIM), Print("   keine Treffer\r\n"), ResetColor);
            lines += 1;
        }
        drawn = lines;
        let _ = out.flush();
        let Ok(Event::Key(k)) = event::read() else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Esc => break None,
            // Approval hotkeys (j/i/n) when the list has no filter typed yet.
            KeyCode::Char('j') | KeyCode::Char('y') if filter.is_empty() && title.is_empty() => break Some(0),
            KeyCode::Char('i') | KeyCode::Char('a') if filter.is_empty() && title.is_empty() => break Some(1),
            KeyCode::Char('n') if filter.is_empty() && title.is_empty() => break None,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break None,
            KeyCode::Enter => break if visible.is_empty() { None } else { Some(sel) },
            KeyCode::Up => {
                if pos > 0 {
                    sel = visible[pos - 1];
                }
            }
            KeyCode::Down => {
                if pos + 1 < visible.len() {
                    sel = visible[pos + 1];
                }
            }
            KeyCode::PageUp => sel = visible[pos.saturating_sub(max_rows)],
            KeyCode::PageDown if !visible.is_empty() => sel = visible[(pos + max_rows).min(visible.len() - 1)],
            KeyCode::Backspace => {
                filter.pop();
            }
            KeyCode::Char(c) => {
                filter.push(c);
            }
            _ => {}
        }
    };
    if drawn > 0 {
        let _ = queue!(out, cursor::MoveUp(drawn as u16), Print("\r"), terminal::Clear(ClearType::FromCursorDown));
    }
    let _ = execute!(out, cursor::Show);
    let _ = terminal::disable_raw_mode();
    result
}

fn numbered_pick(title: &str, items: &[Item]) -> Option<usize> {
    println!("{title}");
    for (i, it) in items.iter().enumerate() {
        println!("  {:>3}) {}  {}", i + 1, it.label, dim(&it.detail));
    }
    let n = prompt_line("Nummer: ")?;
    n.trim().parse::<usize>().ok().filter(|n| *n >= 1 && *n <= items.len()).map(|n| n - 1)
}

pub fn prompt_line(label: &str) -> Option<String> {
    print!("{label}");
    let _ = stdout().flush();
    let mut s = String::new();
    match std::io::stdin().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s.trim_end_matches(['\r', '\n']).to_string()),
    }
}

pub fn prompt_secret(label: &str) -> Option<String> {
    rpassword::prompt_password(label).ok()
}

pub enum Answer {
    Yes,
    Always,
    No,
}

/* ---------------- markdown-lite streaming renderer ---------------- */

#[derive(Default)]
pub struct Markdown {
    buf: String,
    in_code: bool,
    pub wrote_any: bool,
    first: bool,
}

impl Markdown {
    /// Next rendered line gets the "•" bullet (agent message start).
    pub fn start_message(&mut self) {
        self.first = true;
    }

    fn lead(&mut self) -> String {
        if self.first {
            self.first = false;
            format!("{} ", if plain() { "⏺".to_string() } else { "⏺".with(Color::White).to_string() })
        } else {
            "  ".to_string()
        }
    }

    pub fn push(&mut self, s: &str) {
        self.buf.push_str(s);
        while let Some(i) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=i).collect();
            self.render_line(line.trim_end_matches('\n'));
        }
    }

    pub fn finish(&mut self) {
        if !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            self.render_line(&rest);
        }
        self.in_code = false;
    }

    fn render_line(&mut self, line: &str) {
        self.wrote_any = true;
        let t = line.trim_start();
        if t.starts_with("```") {
            self.in_code = !self.in_code;
            let lang = t.trim_start_matches('`').trim();
            let lead = self.lead();
            if self.in_code {
                println!("{lead}{}", dim(&format!("┌─ {}", if lang.is_empty() { "code" } else { lang })));
            } else {
                println!("{lead}{}", dim("└─"));
            }
            return;
        }
        let lead = self.lead();
        if self.in_code {
            println!("{lead}{} {}", "│".with(DIM), line.with(Color::Rgb { r: 200, g: 220, b: 255 }));
            return;
        }
        if let Some(h) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")).or_else(|| t.strip_prefix("# ")) {
            println!("{lead}{}", inline(h).bold());
            return;
        }
        if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            let indent = line.len() - t.len();
            println!("{lead}{}{} {}", " ".repeat(indent), "-".with(DIM), inline(item));
            return;
        }
        println!("{lead}{}", inline(line));
    }
}

/// **bold**, `code` -> ANSI.
fn inline(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    loop {
        let b = rest.find("**");
        let c = rest.find('`');
        match (b, c) {
            (Some(bi), ci) if ci.is_none_or(|ci| bi < ci) => {
                out.push_str(&rest[..bi]);
                let after = &rest[bi + 2..];
                if let Some(end) = after.find("**") {
                    out.push_str(&after[..end].bold().to_string());
                    rest = &after[end + 2..];
                } else {
                    out.push_str("**");
                    rest = after;
                }
            }
            (_, Some(ci)) => {
                out.push_str(&rest[..ci]);
                let after = &rest[ci + 1..];
                if let Some(end) = after.find('`') {
                    out.push_str(&after[..end].with(TEAL).to_string());
                    rest = &after[end + 1..];
                } else {
                    out.push('`');
                    rest = after;
                }
            }
            _ => {
                out.push_str(rest);
                break;
            }
        }
    }
    out
}

/// Usage bar like `████░░░░ 42%`.
pub fn bar(pct: f64, w: usize) -> String {
    let p = pct.clamp(0.0, 100.0);
    let fill = ((p / 100.0) * w as f64).round() as usize;
    let col = if p >= 100.0 { BAD } else if p >= 80.0 { WARN } else { TEAL };
    format!("{}{}", "█".repeat(fill).with(col), "░".repeat(w - fill).with(DIM))
}

pub fn fmt_num(n: f64) -> String {
    if n >= 1e9 {
        format!("{:.2}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.2}M", n / 1e6)
    } else if n >= 1e4 {
        format!("{:.1}k", n / 1e3)
    } else {
        format!("{}", n.round() as i64)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn openrouter_error_is_readable() {
        let raw = r#"Upstream rejected the request: {"error":{"message":"Provider returned error","code":400,"metadata":{"raw":"{\"code\":400,\"msg\":\"bad request\",\"request_id\":\"3946d9fd\"}","provider_name":"AtlasCloud","is_byok":false}},"user_id":"org_2uwFc1"#;
        let p = super::pretty_error(raw);
        assert!(p.contains("AtlasCloud") && p.contains("400") && p.contains("bad request"), "{p}");
    }
}
