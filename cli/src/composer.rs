//! Input box in the style of Claude Code / opencode: a rounded frame that
//! grows with the text, a status line underneath, a slash-command popup,
//! history, bracketed paste and multi-line input.

use crate::ui;
use crossterm::{
    cursor,
    event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{self, ClearType},
};
use std::io::{stdout, Write};
use unicode_width::UnicodeWidthChar;

pub enum Outcome {
    Submit(String),
    Quit,
}

pub struct Composer {
    pub history: Vec<String>,
    pub commands: Vec<(&'static str, &'static str)>,
}

struct State {
    buf: Vec<char>,
    cur: usize,
    hist_pos: Option<usize>,
    draft: Vec<char>,
    popup_sel: usize,
    popup_closed: bool,
    notice: Option<(String, std::time::Instant)>,
    drawn_above_cursor: u16,
    last_ctrl_c: Option<std::time::Instant>,
}

const BORDER: Color = Color::Rgb { r: 88, g: 94, b: 112 };
const BORDER_HOT: Color = Color::Rgb { r: 124, g: 140, b: 255 };

impl Composer {
    pub fn new(commands: Vec<(&'static str, &'static str)>) -> Self {
        let history = std::fs::read_to_string(crate::config::history_path())
            .map(|t| t.lines().filter(|l| !l.is_empty()).map(|l| l.replace("\\n", "\n")).collect())
            .unwrap_or_default();
        Self { history, commands }
    }

    fn save_history(&self) {
        let start = self.history.len().saturating_sub(500);
        let text: String = self.history[start..].iter().map(|h| h.replace('\n', "\\n") + "\n").collect();
        let _ = std::fs::create_dir_all(crate::config::dir());
        let _ = std::fs::write(crate::config::history_path(), text);
    }

    fn matches(&self, st: &State) -> Vec<(&'static str, &'static str)> {
        let text: String = st.buf.iter().collect();
        if st.popup_closed || !text.starts_with('/') || text.contains(' ') || text.contains('\n') {
            return Vec::new();
        }
        self.commands.iter().copied().filter(|(c, _)| c.starts_with(text.as_str())).collect()
    }

    /// Reads one message. `status` is the left part of the status line.
    pub fn read(&mut self, status: &str) -> Outcome {
        let mut st = State {
            buf: Vec::new(),
            cur: 0,
            hist_pos: None,
            draft: Vec::new(),
            popup_sel: 0,
            popup_closed: false,
            notice: None,
            drawn_above_cursor: 0,
            last_ctrl_c: None,
        };
        let mut out = stdout();
        let _ = terminal::enable_raw_mode();
        let _ = queue!(out, EnableBracketedPaste);
        self.draw(&mut st, status, true);
        let result = loop {
            let ev = match event::read() {
                Ok(e) => e,
                Err(_) => break Outcome::Quit,
            };
            match ev {
                Event::Paste(text) => {
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    for ch in text.chars() {
                        st.buf.insert(st.cur, ch);
                        st.cur += 1;
                    }
                }
                Event::Key(k) if k.kind != KeyEventKind::Release => {
                    if let Some(o) = self.key(&mut st, k) {
                        break o;
                    }
                }
                Event::Resize(_, _) => {}
                _ => continue,
            }
            self.draw(&mut st, status, true);
        };
        // Replace the box by the submitted message (or nothing).
        self.clear(&mut st);
        let _ = queue!(out, DisableBracketedPaste);
        let _ = out.flush();
        let _ = terminal::disable_raw_mode();
        if let Outcome::Submit(text) = &result {
            print_user_message(text);
            if self.history.last() != Some(text) {
                self.history.push(text.clone());
                self.save_history();
            }
        }
        result
    }

    fn key(&mut self, st: &mut State, k: KeyEvent) -> Option<Outcome> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        let popup = self.matches(st);
        if !matches!(k.code, KeyCode::Char('c') if ctrl) {
            st.last_ctrl_c = None;
        }
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if !st.buf.is_empty() {
                    st.buf.clear();
                    st.cur = 0;
                    return None;
                }
                if st.last_ctrl_c.is_some_and(|t| t.elapsed().as_secs() < 3) {
                    return Some(Outcome::Quit);
                }
                st.last_ctrl_c = Some(std::time::Instant::now());
                st.notice = Some(("Erneut Ctrl+C drücken zum Beenden".into(), std::time::Instant::now()));
            }
            KeyCode::Char('d') if ctrl => {
                if st.buf.is_empty() {
                    return Some(Outcome::Quit);
                }
                if st.cur < st.buf.len() {
                    st.buf.remove(st.cur);
                }
            }
            // Newline: Shift/Alt+Enter, Ctrl+J, or a trailing backslash.
            KeyCode::Enter if shift || alt => self.insert(st, '\n'),
            KeyCode::Char('j') if ctrl => self.insert(st, '\n'),
            KeyCode::Enter => {
                if !popup.is_empty() {
                    let (cmd, _) = popup[st.popup_sel.min(popup.len() - 1)];
                    return Some(Outcome::Submit(cmd.to_string()));
                }
                if st.cur > 0 && st.buf.get(st.cur - 1) == Some(&'\\') {
                    st.buf[st.cur - 1] = '\n';
                    return None;
                }
                let text: String = st.buf.iter().collect::<String>().trim().to_string();
                if text.is_empty() {
                    return None;
                }
                return Some(Outcome::Submit(text));
            }
            KeyCode::Tab => {
                if !popup.is_empty() {
                    let (cmd, _) = popup[st.popup_sel.min(popup.len() - 1)];
                    st.buf = format!("{cmd} ").chars().collect();
                    st.cur = st.buf.len();
                }
            }
            KeyCode::Esc => {
                if !popup.is_empty() {
                    st.popup_closed = true;
                } else if !st.buf.is_empty() {
                    st.buf.clear();
                    st.cur = 0;
                }
            }
            KeyCode::Up if !popup.is_empty() => st.popup_sel = st.popup_sel.saturating_sub(1),
            KeyCode::Down if !popup.is_empty() => st.popup_sel = (st.popup_sel + 1).min(popup.len() - 1),
            KeyCode::Up => {
                // Move inside multi-line text first, then through history.
                if st.buf[..st.cur].contains(&'\n') {
                    let line_start = st.buf[..st.cur].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
                    let col = st.cur - line_start;
                    let prev_start = st.buf[..line_start - 1].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
                    st.cur = (prev_start + col).min(line_start - 1);
                } else if !self.history.is_empty() {
                    let pos = match st.hist_pos {
                        None => {
                            st.draft = st.buf.clone();
                            self.history.len() - 1
                        }
                        Some(p) => p.saturating_sub(1),
                    };
                    st.hist_pos = Some(pos);
                    st.buf = self.history[pos].chars().collect();
                    st.cur = st.buf.len();
                }
            }
            KeyCode::Down => {
                if st.buf[st.cur..].contains(&'\n') {
                    let line_start = st.buf[..st.cur].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0);
                    let col = st.cur - line_start;
                    let next_start = st.cur + st.buf[st.cur..].iter().position(|c| *c == '\n').unwrap() + 1;
                    let next_end = next_start + st.buf[next_start..].iter().position(|c| *c == '\n').unwrap_or(st.buf.len() - next_start);
                    st.cur = (next_start + col).min(next_end);
                } else if let Some(p) = st.hist_pos {
                    if p + 1 < self.history.len() {
                        st.hist_pos = Some(p + 1);
                        st.buf = self.history[p + 1].chars().collect();
                    } else {
                        st.hist_pos = None;
                        st.buf = std::mem::take(&mut st.draft);
                    }
                    st.cur = st.buf.len();
                }
            }
            KeyCode::Left => st.cur = st.cur.saturating_sub(1),
            KeyCode::Right => st.cur = (st.cur + 1).min(st.buf.len()),
            KeyCode::Home => st.cur = st.buf[..st.cur].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0),
            KeyCode::Char('a') if ctrl => st.cur = st.buf[..st.cur].iter().rposition(|c| *c == '\n').map(|i| i + 1).unwrap_or(0),
            KeyCode::End => st.cur = st.cur + st.buf[st.cur..].iter().position(|c| *c == '\n').unwrap_or(st.buf.len() - st.cur),
            KeyCode::Char('e') if ctrl => st.cur = st.cur + st.buf[st.cur..].iter().position(|c| *c == '\n').unwrap_or(st.buf.len() - st.cur),
            KeyCode::Char('u') if ctrl => {
                st.buf.drain(..st.cur);
                st.cur = 0;
            }
            KeyCode::Char('k') if ctrl => {
                st.buf.truncate(st.cur);
            }
            KeyCode::Char('w') if ctrl => {
                let mut i = st.cur;
                while i > 0 && st.buf[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && st.buf[i - 1] != ' ' && st.buf[i - 1] != '\n' {
                    i -= 1;
                }
                st.buf.drain(i..st.cur);
                st.cur = i;
            }
            KeyCode::Char('l') if ctrl => {
                let _ = queue!(stdout(), terminal::Clear(ClearType::All), cursor::MoveTo(0, 0));
                st.drawn_above_cursor = 0;
            }
            KeyCode::Backspace => {
                if st.cur > 0 {
                    st.cur -= 1;
                    st.buf.remove(st.cur);
                }
            }
            KeyCode::Delete => {
                if st.cur < st.buf.len() {
                    st.buf.remove(st.cur);
                }
            }
            KeyCode::Char(ch) if !ctrl => self.insert(st, ch),
            _ => {}
        }
        if !matches!(k.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Tab | KeyCode::Esc) {
            st.popup_closed = st.popup_closed && st.buf.first() == Some(&'/');
            st.popup_sel = 0;
        }
        None
    }

    fn insert(&self, st: &mut State, ch: char) {
        st.buf.insert(st.cur, ch);
        st.cur += 1;
    }

    fn clear(&self, st: &mut State) {
        let mut out = stdout();
        if st.drawn_above_cursor > 0 {
            let _ = queue!(out, cursor::MoveUp(st.drawn_above_cursor));
        }
        let _ = queue!(out, Print("\r"), terminal::Clear(ClearType::FromCursorDown));
        st.drawn_above_cursor = 0;
    }

    fn draw(&self, st: &mut State, status: &str, focused: bool) {
        let w = ui::width().max(30);
        let inner = w - 4; // "│ " + text + " │"
        let text_w = inner - 2; // "› " prefix
        // Wrap text into visual rows, tracking where the cursor lands.
        let mut rows: Vec<String> = vec![String::new()];
        let mut row_w = 0usize;
        let (mut cur_row, mut cur_col) = (0usize, 0usize);
        for (i, ch) in st.buf.iter().enumerate() {
            if i == st.cur {
                cur_row = rows.len() - 1;
                cur_col = row_w;
            }
            if *ch == '\n' {
                rows.push(String::new());
                row_w = 0;
                continue;
            }
            let cw = ch.width().unwrap_or(1);
            if row_w + cw > text_w {
                rows.push(String::new());
                row_w = 0;
            }
            rows.last_mut().unwrap().push(*ch);
            row_w += cw;
        }
        if st.cur == st.buf.len() {
            if row_w >= text_w {
                rows.push(String::new());
                row_w = 0;
            }
            cur_row = rows.len() - 1;
            cur_col = row_w;
        }
        // Window of at most 12 rows around the cursor.
        let max_rows = 12usize;
        let first = cur_row.saturating_sub(max_rows - 1);
        let visible: Vec<&String> = rows.iter().skip(first).take(max_rows).collect();

        self.clear(st);
        let mut out = stdout();
        let bc = if focused { BORDER_HOT } else { BORDER };
        let line = "─".repeat(w - 2);
        let _ = queue!(out, SetForegroundColor(bc), Print(format!("╭{line}╮\r\n")));
        let empty = st.buf.is_empty();
        for (i, r) in visible.iter().enumerate() {
            let prefix = if i + first == 0 { "› " } else { "  " };
            let _ = queue!(out, SetForegroundColor(bc), Print("│ "), ResetColor);
            let _ = queue!(out, SetForegroundColor(if i + first == 0 { BORDER_HOT } else { BORDER }), SetAttribute(Attribute::Bold), Print(prefix), SetAttribute(Attribute::Reset));
            let used: usize;
            if empty {
                let ph = "Schreib eine Aufgabe …  (/ für Befehle, @datei zum Anhängen)";
                let ph: String = ph.chars().take(text_w).collect();
                used = ph.chars().count();
                let _ = queue!(out, SetForegroundColor(ui::DIM), Print(&ph), ResetColor);
            } else {
                used = r.chars().map(|c| c.width().unwrap_or(1)).sum();
                let _ = queue!(out, Print(r.as_str()));
            }
            let pad = text_w.saturating_sub(used);
            let _ = queue!(out, Print(" ".repeat(pad)), SetForegroundColor(bc), Print(" │\r\n"));
        }
        let _ = queue!(out, Print(format!("╰{line}╯")), ResetColor);
        let mut below = 1u16; // rows printed after the bottom border line
        // Slash-command popup.
        let popup = self.matches(st);
        for (i, (c, d)) in popup.iter().take(8).enumerate() {
            let sel = i == st.popup_sel.min(popup.len().saturating_sub(1));
            let _ = queue!(out, Print("\r\n  "));
            if sel {
                let _ = queue!(out, SetBackgroundColor(Color::Rgb { r: 40, g: 46, b: 72 }), SetForegroundColor(Color::White));
            } else {
                let _ = queue!(out, SetForegroundColor(BORDER_HOT));
            }
            let _ = queue!(out, Print(format!(" {:<22}", c)), SetForegroundColor(if sel { Color::White } else { ui::DIM }), Print(format!("{} ", d)), ResetColor);
            below += 1;
        }
        // Status line.
        let notice = st.notice.as_ref().filter(|(_, t)| t.elapsed().as_secs() < 3).map(|(n, _)| n.clone());
        let right = notice.clone().unwrap_or_else(|| "⏎ senden · ⇧⏎ neue Zeile · ↑ Verlauf · ⌃C beenden".into());
        let left_vis: usize = strip_ansi_width(status);
        let gap = w.saturating_sub(left_vis + right.chars().count() + 3);
        let _ = queue!(out, Print("\r\n  "), Print(status), Print(" ".repeat(gap)));
        let _ = queue!(out, SetForegroundColor(if notice.is_some() { ui::WARN } else { ui::DIM }), Print(&right), ResetColor);
        below += 1;
        // Put the cursor into the box.
        let box_rows = visible.len() as u16; // text rows
        let cursor_row_from_top = 1 + (cur_row - first) as u16; // 1 = top border
        let total_rows = 1 + box_rows + below; // top + text + bottom/popup/status (bottom counted in below? no)
        // Rows from cursor row to the last printed row:
        let last_row_index = 1 + box_rows + below - 1; // index of status line (0-based from top)
        let up = last_row_index - cursor_row_from_top;
        let _ = total_rows;
        let _ = queue!(out, cursor::MoveUp(up), Print("\r"), cursor::MoveRight((4 + cur_col) as u16));
        st.drawn_above_cursor = cursor_row_from_top;
        let _ = out.flush();
    }
}

fn strip_ansi_width(s: &str) -> usize {
    let mut w = 0;
    let mut esc = false;
    for ch in s.chars() {
        if esc {
            if ch.is_ascii_alphabetic() {
                esc = false;
            }
            continue;
        }
        if ch == '\x1b' {
            esc = true;
            continue;
        }
        w += ch.width().unwrap_or(0);
    }
    w
}

/// Echo of a sent message: subtle panel with a left accent bar.
pub fn print_user_message(text: &str) {
    let w = ui::width();
    for (i, line) in text.lines().enumerate() {
        let l: String = line.chars().take(w.saturating_sub(4)).collect();
        if ui::PLAIN.load(std::sync::atomic::Ordering::Relaxed) {
            println!("{} {l}", if i == 0 { ">" } else { " " });
        } else {
            let mut out = stdout();
            let _ = queue!(
                out,
                SetForegroundColor(BORDER_HOT),
                Print("▌ "),
                SetForegroundColor(Color::Rgb { r: 200, g: 204, b: 216 }),
                Print(&l),
                ResetColor,
                Print("\n")
            );
            let _ = out.flush();
        }
    }
}
