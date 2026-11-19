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
        let title = format!(">_ Multi LLM (v{})", env!("CARGO_PKG_VERSION"));
        v.push((if plain() { title.clone() } else { format!("{} {}", ">_".with(DIM), format!("Multi LLM (v{})", env!("CARGO_PKG_VERSION")).bold()) }, title.width()));
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
    println!("{}", dim(&format!("╭{line}╮")));
    for (txt, w) in rows {
        let pad = inner.saturating_sub(w + 1);
        println!("{} {}{}{}", dim("│"), txt, " ".repeat(pad), dim("│"));
    }
    println!("{}", dim(&format!("╰{line}╯")));
    println!();
    println!("  {}", dim("Beschreibe eine Aufgabe oder nutze einen Befehl:"));
    for (c, d) in [("/model", "Modell wählen"), ("/connect", "Provider hinzufügen"), ("/usage", "Nutzung & Limits"), ("/help", "alle Befehle")] {
        println!("  {} {}", c.with(ACCENT), dim(&format!("- {d}")));
    }
    println!();
}

/// Put the terminal back into a sane state (raw mode off, cursor visible).
pub fn restore_terminal() {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(stdout(), cursor::Show, ResetColor);
}

/// "■ Unterbrochen" after Ctrl+C.
pub fn interrupted() {
    println!("{} {}", bad("■"), dim("Unterbrochen – was soll ich stattdessen tun?"));
}

/// Codex-like footer after a turn: elapsed time and context left.
pub fn turn_footer(secs: u64, ctx_used: u64, ctx_len: Option<u64>) {
    let mut parts = vec![format!("{secs}s")];
    if let Some(len) = ctx_len.filter(|l| *l > 0) {
        let left = 100.0 - (ctx_used as f64 / len as f64 * 100.0).min(100.0);
        parts.push(format!("{left:.0}% Kontext frei"));
    } else if ctx_used > 0 {
        parts.push(format!("{} Tokens im Kontext", fmt_num(ctx_used as f64)));
    }
    println!();
    println!("{}", dim(&format!("─ {} ─", parts.join(" · "))));
}

/// "• Ran cargo test" (verb bold). `pending` = waiting for approval.
pub fn tool_line(verb: &str, detail: &str, pending: bool) {
    let bullet = if pending { warn("•") } else { ok("•") };
    println!("{} {} {}", bullet, bold(verb), detail);
}

/// Indented tool output: "  └ first line" + "    next lines".
pub fn tool_out(lines: &[String]) {
    for (i, l) in lines.iter().enumerate() {
        println!("  {} {}", if i == 0 { dim("└") } else { " ".to_string() }, l);
    }
}

/// "• Edited src/x.rs (+3 −1)" plus a short colored diff.
pub fn diff_block(verb: &str, path: &str, before: &str, after: &str) {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    // common prefix / suffix -> changed middle block
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
    let stats = format!("({} {})", ok(&format!("+{}", added.len())), bad(&format!("−{}", removed.len())));
    println!("{} {} {} {}", ok("•"), bold(verb), path, stats);
    let maxw = width().saturating_sub(12);
    let cut = |l: &str| l.chars().take(maxw).collect::<String>();
    let mut shown = 0;
    let limit = 14;
    let start = pre.saturating_sub(1);
    if start < pre && start < old.len() {
        println!("    {} {}", dim(&format!("{:>4}", start + 1)), dim(&format!("  {}", cut(old[start]))));
    }
    for (i, l) in removed.iter().enumerate() {
        if shown >= limit { break; }
        println!("    {} {}", dim(&format!("{:>4}", pre + i + 1)), bad(&format!("- {}", cut(l))));
        shown += 1;
    }
    for (i, l) in added.iter().enumerate() {
        if shown >= limit { break; }
        println!("    {} {}", dim(&format!("{:>4}", pre + i + 1)), ok(&format!("+ {}", cut(l))));
        shown += 1;
    }
    let total = removed.len() + added.len();
    if total > shown {
        println!("    {}", dim(&format!("… +{} weitere Zeilen", total - shown)));
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
            let mut i = 0usize;
            while !s2.load(Ordering::Relaxed) {
                let secs = started.elapsed().as_secs();
                // Codex-like shimmer: one bright letter walking through the label.
                let chars: Vec<char> = label.chars().collect();
                let hot = (i / 2) % (chars.len() + 6);
                let mut out = stdout();
                let _ = queue!(out, Print("\r"), SetForegroundColor(if i % 10 < 5 { TEAL } else { DIM }), Print("• "));
                for (k, ch) in chars.iter().enumerate() {
                    let c = if k == hot || k + 1 == hot { Color::White } else { DIM };
                    let _ = queue!(out, SetForegroundColor(c), Print(ch));
                }
                let _ = queue!(
                    out,
                    SetForegroundColor(DIM),
                    Print(format!(" ({secs}s • Ctrl+C zum Unterbrechen)")),
                    ResetColor,
                    terminal::Clear(ClearType::UntilNewLine)
                );
                let _ = out.flush();
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
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
            format!("{} ", if plain() { "•".to_string() } else { "•".with(Color::White).to_string() })
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
