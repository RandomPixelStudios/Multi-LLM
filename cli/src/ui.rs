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

pub fn dim(s: &str) -> String {
    s.with(DIM).to_string()
}
pub fn bold(s: &str) -> String {
    s.bold().to_string()
}
pub fn ok(s: &str) -> String {
    s.with(OK).to_string()
}
pub fn warn(s: &str) -> String {
    s.with(WARN).to_string()
}
pub fn bad(s: &str) -> String {
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

pub fn banner(host: &str, user: &str, model: &str, mode: &str) {
    let w = width().min(78);
    let line = "─".repeat(w.saturating_sub(2));
    println!("{}", format!("╭{line}╮").with(ACCENT));
    let row = |s: String, vis: usize| {
        let pad = w.saturating_sub(4 + vis);
        println!("{} {}{} {}", "│".with(ACCENT), s, " ".repeat(pad), "│".with(ACCENT));
    };
    let title = format!("{} {}", "✻".with(TEAL), "Multi LLM".bold());
    row(title, 11);
    row(String::new(), 0);
    let l1 = format!("{}{}", "Host:   ".with(DIM), host);
    row(l1, 8 + host.width());
    let l2 = format!("{}{}", "User:   ".with(DIM), user);
    row(l2, 8 + user.width());
    let l3 = format!("{}{}", "Model:  ".with(DIM), model);
    row(l3, 8 + model.width());
    let mode_s = match mode {
        "bypass" => "bypass-permissions".with(BAD).to_string(),
        "yolo" => "yolo".with(WARN).to_string(),
        _ => "ask".with(OK).to_string(),
    };
    let mode_len = match mode { "bypass" => 18, "yolo" => 4, _ => 3 };
    row(format!("{}{}", "Mode:   ".with(DIM), mode_s), 8 + mode_len);
    println!("{}", format!("╰{line}╯").with(ACCENT));
    println!("{}", dim("  /help für Befehle · Ctrl+C bricht ab · Ctrl+D beendet"));
    println!();
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
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let started = std::time::Instant::now();
            let mut i = 0;
            while !s2.load(Ordering::Relaxed) {
                let secs = started.elapsed().as_secs();
                let mut out = stdout();
                let _ = queue!(
                    out,
                    Print("\r"),
                    SetForegroundColor(ACCENT),
                    Print(frames[i % frames.len()]),
                    ResetColor,
                    Print(format!(" {} ", label)),
                    SetForegroundColor(DIM),
                    Print(format!("({secs}s · Ctrl+C bricht ab)")),
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

/// Permission question: [y]es / [a]lways for this session / [n]o.
pub fn confirm(question: &str) -> Answer {
    println!(
        "  {} {}  {}",
        "?".with(WARN).bold(),
        question,
        dim("[j]a · [i]mmer · [n]ein")
    );
    if !std::io::stdin().is_terminal() {
        return Answer::No;
    }
    if terminal::enable_raw_mode().is_err() {
        let a = prompt_line("> ").unwrap_or_default().to_lowercase();
        return match a.as_str() {
            "j" | "y" | "ja" | "yes" => Answer::Yes,
            "i" | "a" => Answer::Always,
            _ => Answer::No,
        };
    }
    let ans = loop {
        let Ok(Event::Key(k)) = event::read() else { continue };
        if k.kind != KeyEventKind::Press {
            continue;
        }
        match k.code {
            KeyCode::Char('j') | KeyCode::Char('y') | KeyCode::Enter => break Answer::Yes,
            KeyCode::Char('i') | KeyCode::Char('a') => break Answer::Always,
            KeyCode::Char('n') | KeyCode::Esc => break Answer::No,
            KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => break Answer::No,
            _ => {}
        }
    };
    let _ = terminal::disable_raw_mode();
    ans
}

/* ---------------- markdown-lite streaming renderer ---------------- */

#[derive(Default)]
pub struct Markdown {
    buf: String,
    in_code: bool,
    pub wrote_any: bool,
}

impl Markdown {
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
            if self.in_code {
                println!("  {}", dim(&format!("┌─ {}", if lang.is_empty() { "code" } else { lang })));
            } else {
                println!("  {}", dim("└─"));
            }
            return;
        }
        if self.in_code {
            println!("  {} {}", "│".with(DIM), line.with(Color::Rgb { r: 200, g: 220, b: 255 }));
            return;
        }
        if let Some(h) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")).or_else(|| t.strip_prefix("# ")) {
            println!("  {}", inline(h).bold().with(ACCENT));
            return;
        }
        if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            let indent = line.len() - t.len();
            println!("  {}{} {}", " ".repeat(indent), "•".with(TEAL), inline(item));
            return;
        }
        println!("  {}", inline(line));
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
