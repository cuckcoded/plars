//! Shared helpers: tinymist lookup, URIs, LSP <-> GtkTextIter positions.

use std::path::{Path, PathBuf};

use gtk::prelude::*;
use gtk::{gio, glib};
use serde_json::Value;

pub fn find_tinymist(configured: &str) -> Option<String> {
    let env = std::env::var("PLARS_TINYMIST").unwrap_or_default();
    for cand in [configured, env.as_str()] {
        if !cand.is_empty() && is_executable(Path::new(cand)) {
            return Some(cand.to_owned());
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let cand = dir.join(TINYMIST);
            if is_executable(&cand) {
                return Some(cand.to_string_lossy().into_owned());
            }
        }
    }
    let home = glib::home_dir();
    for dir in [".local/bin", ".cargo/bin"] {
        let cand = home.join(dir).join(TINYMIST);
        if is_executable(&cand) {
            return Some(cand.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

const TINYMIST: &str = if cfg!(windows) { "tinymist.exe" } else { "tinymist" };

pub fn path_to_uri(path: &Path) -> String {
    gio::File::for_path(path).uri().to_string()
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    gio::File::for_uri(uri).path()
}

pub fn is_typst(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "typ")
}

// LSP positions are (line, UTF-16 code unit); GTK iters count code points.

pub fn lsp_position(buf: &impl IsA<gtk::TextBuffer>, it: &gtk::TextIter) -> Value {
    let mut start = *it;
    start.set_line_offset(0);
    let text = buf.as_ref().text(&start, it, true);
    let units: usize = text.chars().map(char::len_utf16).sum();
    serde_json::json!({ "line": it.line(), "character": units })
}

pub fn iter_at_lsp(buf: &impl IsA<gtk::TextBuffer>, line: i64, character: i64) -> gtk::TextIter {
    let buf = buf.as_ref();
    if line >= buf.line_count() as i64 {
        return buf.end_iter();
    }
    let Some(mut it) = buf.iter_at_line(line.max(0) as i32) else { return buf.end_iter() };
    let mut end = it;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    let text = buf.text(&it, &end, true);
    let (mut units, mut chars) = (0i64, 0i32);
    for ch in text.chars() {
        if units >= character {
            break;
        }
        units += ch.len_utf16() as i64;
        chars += 1;
    }
    it.set_line_offset(chars);
    it
}

pub fn pos_of(v: &Value) -> (i64, i64) {
    (v["line"].as_i64().unwrap_or(0), v["character"].as_i64().unwrap_or(0))
}

pub fn range_iters(buf: &impl IsA<gtk::TextBuffer>, range: &Value) -> (gtk::TextIter, gtk::TextIter) {
    let (sl, sc) = pos_of(&range["start"]);
    let (el, ec) = pos_of(&range["end"]);
    (iter_at_lsp(buf, sl, sc), iter_at_lsp(buf, el, ec))
}

/// Apply LSP TextEdits bottom-most first so earlier ranges stay valid.
pub fn apply_text_edits(buf: &impl IsA<gtk::TextBuffer>, edits: &[Value]) {
    let buf = buf.as_ref();
    let mut edits: Vec<&Value> = edits.iter().collect();
    edits.sort_by_key(|e| pos_of(&e["range"]["start"]));
    buf.begin_user_action();
    for e in edits.into_iter().rev() {
        let (mut s, mut en) = range_iters(buf, &e["range"]);
        let mark = buf.create_mark(None, &s, true);
        buf.delete(&mut s, &mut en);
        let mut at = buf.iter_at_mark(&mark);
        buf.insert(&mut at, e["newText"].as_str().unwrap_or_default());
        buf.delete_mark(&mark);
    }
    buf.end_user_action();
}

/// Just enough Markdown for tinymist hovers: fenced code, `code`, **bold**, headings.
pub fn markdown_to_pango(md: &str) -> String {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in md.lines() {
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        let esc = glib::markup_escape_text(line).to_string();
        if in_code {
            out.push(format!("<tt>{esc}</tt>"));
            continue;
        }
        let esc = wrap_alternate(&esc, "`", "tt");
        let esc = wrap_alternate(&esc, "**", "b");
        if line.starts_with('#') {
            out.push(format!("<b>{}</b>", esc.trim_start_matches('#').trim()));
        } else if matches!(line.trim(), "---" | "***") {
            out.push(String::new());
        } else {
            out.push(esc);
        }
    }
    out.join("\n").trim().to_owned()
}

fn wrap_alternate(s: &str, delim: &str, tag: &str) -> String {
    let parts: Vec<&str> = s.split(delim).collect();
    if parts.len().is_multiple_of(2) {
        return s.to_owned(); // unbalanced
    }
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| if i % 2 == 1 { format!("<{tag}>{p}</{tag}>") } else { (*p).to_owned() })
        .collect()
}

pub fn basename(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

pub fn relative(p: &Path, root: Option<&Path>) -> String {
    root.and_then(|r| p.strip_prefix(r).ok())
        .unwrap_or(p)
        .to_string_lossy()
        .into_owned()
}
