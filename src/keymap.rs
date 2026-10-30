//! Editor keymaps modelled on other editors. A keymap is a table of key
//! sequences (one or more chords, e.g. Emacs `C-x C-s`) bound to commands.
//! Commands named `win.*`/`app.*` are GActions; everything else is an editor
//! command run by `EditorPage::run_command`.
//!
//! Single-chord action bindings also become application accelerators so they
//! work outside the editor; base accelerators whose key a keymap uses for
//! something else are dropped.

use gtk::gdk;
use gtk::prelude::*;

pub type Chord = (gdk::Key, gdk::ModifierType);

pub struct Keymap {
    pub id: &'static str,
    pub name: &'static str,
    /// Modifier that turns a primary click into "add a caret here".
    pub add_cursor_click: gdk::ModifierType,
    /// Modifier that turns a primary-button drag into a block (column)
    /// selection; middle-button drags always do.
    pub box_drag: gdk::ModifierType,
    /// Whether Ctrl+Click goes to the definition (Sublime uses it for carets).
    pub ctrl_click_definition: bool,
    pub vim: bool,
    bindings: &'static [(&'static str, &'static str)],
}

pub enum Lookup {
    Command(&'static str),
    Prefix,
    None,
}

/// Accelerators every keymap starts from.
const BASE_ACCELS: &[(&str, &[&str])] = &[
    ("win.open-file", &["<Ctrl>o"]),
    ("win.open-folder", &["<Ctrl><Shift>o"]),
    ("win.new-file", &["<Ctrl>n"]),
    ("win.save", &["<Ctrl>s"]),
    ("win.save-all", &["<Ctrl><Shift>s"]),
    ("win.close-tab", &["<Ctrl>w"]),
    ("win.find", &["<Ctrl>f"]),
    ("win.replace", &["<Ctrl>h"]),
    ("win.find-project", &["<Ctrl><Shift>f"]),
    ("win.format", &["<Ctrl><Shift>i"]),
    ("win.goto-definition", &["F12"]),
    ("win.toggle-sidebar", &["<Ctrl>b"]),
    ("win.toggle-panel", &["<Ctrl>j"]),
    ("win.toggle-preview", &["<Ctrl><Alt>p"]),
    ("win.export::pdf", &["<Ctrl>e"]),
    ("win.docs", &["F1"]),
    ("win.zoom-in", &["<Ctrl>plus", "<Ctrl>equal"]),
    ("win.zoom-out", &["<Ctrl>minus"]),
    ("win.toggle-breakpoint", &["F9"]),
    ("win.debug-continue", &["F5"]),
    ("win.debug-stop", &["<Shift>F5"]),
    ("win.debug-next", &["F10"]),
    ("win.debug-in", &["F11"]),
    ("win.debug-out", &["<Shift>F11"]),
    ("win.next-tab", &["<Ctrl>Page_Down"]),
    ("win.prev-tab", &["<Ctrl>Page_Up"]),
    ("win.shortcuts", &["<Ctrl>question"]),
    ("app.preferences", &["<Ctrl>comma"]),
    ("app.quit", &["<Ctrl>q"]),
    ("app.new-window", &["<Ctrl><Shift>n"]),
];

const DEFAULT: &[(&str, &str)] = &[
    ("<Ctrl>d", "cursor.add-next-match"),
    ("<Ctrl><Shift>l", "cursor.select-all-matches"),
    ("<Ctrl><Alt>Up", "cursor.add-above"),
    ("<Ctrl><Alt>Down", "cursor.add-below"),
    ("<Ctrl><Shift>d", "line.duplicate"),
    ("<Ctrl><Shift>k", "line.delete"),
    ("<Alt>Up", "line.move-up"),
    ("<Alt>Down", "line.move-down"),
    ("<Ctrl>Return", "line.insert-below"),
    ("<Ctrl><Shift>Return", "line.insert-above"),
    ("<Ctrl>l", "line.select"),
    ("<Ctrl>slash", "comment.line"),
    ("<Ctrl><Shift>slash", "comment.block"),
    ("<Ctrl>bracketright", "edit.indent"),
    ("<Ctrl>bracketleft", "edit.outdent"),
    ("<Ctrl>m", "goto.bracket"),
    ("<Ctrl>g", "goto.line"),
    ("<Shift><Alt>Right", "select.expand"),
    ("<Shift><Alt>Left", "select.shrink"),
    ("<Ctrl>x", "edit.cut"),
    ("<Ctrl>c", "edit.copy"),
    ("<Ctrl>v", "edit.paste"),
    ("<Ctrl>z", "edit.undo"),
    ("<Ctrl><Shift>z", "edit.redo"),
];

const VSCODE: &[(&str, &str)] = &[
    // multiple cursors
    ("<Ctrl>d", "cursor.add-next-match"),
    ("<Ctrl>k <Ctrl>d", "cursor.skip-match"),
    ("<Ctrl><Shift>l", "cursor.select-all-matches"),
    ("<Ctrl>F2", "cursor.select-all-matches"),
    ("<Ctrl><Shift>Up", "cursor.add-above"),
    ("<Ctrl><Shift>Down", "cursor.add-below"),
    ("<Ctrl><Alt>Up", "cursor.add-above"),
    ("<Ctrl><Alt>Down", "cursor.add-below"),
    ("<Shift><Alt>i", "cursor.line-ends"),
    ("<Ctrl>u", "cursor.undo"),
    // lines
    ("<Ctrl><Shift>k", "line.delete"),
    ("<Alt>Up", "line.move-up"),
    ("<Alt>Down", "line.move-down"),
    ("<Shift><Alt>Up", "line.copy-up"),
    ("<Shift><Alt>Down", "line.copy-down"),
    ("<Ctrl>Return", "line.insert-below"),
    ("<Ctrl><Shift>Return", "line.insert-above"),
    ("<Ctrl>l", "line.select"),
    ("<Ctrl>slash", "comment.line"),
    ("<Ctrl>k <Ctrl>c", "comment.line"),
    ("<Ctrl>k <Ctrl>u", "comment.line"),
    ("<Shift><Alt>a", "comment.block"),
    ("<Ctrl>bracketright", "edit.indent"),
    ("<Ctrl>bracketleft", "edit.outdent"),
    ("<Ctrl><Shift>backslash", "goto.bracket"),
    ("<Ctrl>g", "goto.line"),
    ("<Shift><Alt>Right", "select.expand"),
    ("<Shift><Alt>Left", "select.shrink"),
    ("<Ctrl>x", "edit.cut"),
    ("<Ctrl>c", "edit.copy"),
    ("<Ctrl>v", "edit.paste"),
    ("<Ctrl>z", "edit.undo"),
    ("<Ctrl>y", "edit.redo"),
    ("<Ctrl><Shift>z", "edit.redo"),
    // workbench
    ("<Ctrl>p", "win.open-file"),
    ("<Ctrl><Shift>e", "win.toggle-sidebar"),
    ("<Ctrl>grave", "win.toggle-panel"),
    ("<Ctrl>k v", "win.toggle-preview"),
    ("<Ctrl>k <Ctrl>s", "win.shortcuts"),
    ("<Ctrl>k <Ctrl>w", "win.close-tab"),
];

const JETBRAINS: &[(&str, &str)] = &[
    // multiple carets
    ("<Alt>j", "cursor.add-next-match"),
    ("<Shift><Alt>j", "cursor.undo"),
    ("<Ctrl><Shift><Alt>j", "cursor.select-all-matches"),
    ("<Ctrl><Shift><Alt>Up", "cursor.add-above"),
    ("<Ctrl><Shift><Alt>Down", "cursor.add-below"),
    // editing
    ("<Ctrl>d", "line.duplicate"),
    ("<Ctrl>y", "line.delete"),
    ("<Shift><Alt>Up", "line.move-up"),
    ("<Shift><Alt>Down", "line.move-down"),
    ("<Ctrl><Shift>Up", "line.move-up"),
    ("<Ctrl><Shift>Down", "line.move-down"),
    ("<Shift>Return", "line.insert-below"),
    ("<Ctrl><Alt>Return", "line.insert-above"),
    ("<Ctrl><Shift>j", "line.join"),
    ("<Ctrl>slash", "comment.line"),
    ("<Ctrl><Shift>slash", "comment.block"),
    ("<Ctrl>w", "select.expand"),
    ("<Ctrl><Shift>w", "select.shrink"),
    ("<Ctrl><Shift>m", "goto.bracket"),
    ("<Ctrl>g", "goto.line"),
    ("<Ctrl>x", "edit.cut"),
    ("<Ctrl>c", "edit.copy"),
    ("<Ctrl>v", "edit.paste"),
    ("<Ctrl>z", "edit.undo"),
    ("<Ctrl><Shift>z", "edit.redo"),
    // IDE
    ("<Ctrl><Alt>l", "win.format"),
    ("<Ctrl>b", "win.goto-definition"),
    ("<Ctrl>r", "win.replace"),
    ("<Ctrl><Shift>r", "win.find-project"),
    ("<Ctrl>F4", "win.close-tab"),
    ("<Alt>1", "win.toggle-sidebar"),
    ("<Alt>4", "win.toggle-panel"),
    ("<Ctrl><Alt>s", "app.preferences"),
    ("<Ctrl>F8", "win.toggle-breakpoint"),
    ("<Shift>F9", "win.debug-continue"),
    ("F8", "win.debug-next"),
    ("F7", "win.debug-in"),
    ("<Shift>F8", "win.debug-out"),
    ("<Ctrl>F2", "win.debug-stop"),
];

const SUBLIME: &[(&str, &str)] = &[
    ("<Ctrl>d", "cursor.add-next-match"),
    ("<Ctrl>k <Ctrl>d", "cursor.skip-match"),
    ("<Alt>F3", "cursor.select-all-matches"),
    ("<Ctrl><Shift>l", "cursor.split-lines"),
    ("<Shift><Alt>Up", "cursor.add-above"),
    ("<Shift><Alt>Down", "cursor.add-below"),
    ("<Ctrl>u", "cursor.undo"),
    ("<Ctrl><Shift>d", "line.duplicate"),
    ("<Ctrl><Shift>k", "line.delete"),
    ("<Ctrl><Shift>Up", "line.move-up"),
    ("<Ctrl><Shift>Down", "line.move-down"),
    ("<Ctrl>Return", "line.insert-below"),
    ("<Ctrl><Shift>Return", "line.insert-above"),
    ("<Ctrl>j", "line.join"),
    ("<Ctrl>l", "line.select"),
    ("<Ctrl>slash", "comment.line"),
    ("<Ctrl><Shift>slash", "comment.block"),
    ("<Ctrl>bracketright", "edit.indent"),
    ("<Ctrl>bracketleft", "edit.outdent"),
    ("<Ctrl>m", "goto.bracket"),
    ("<Ctrl><Shift>m", "select.expand"),
    ("<Ctrl><Shift>space", "select.expand"),
    ("<Ctrl>g", "goto.line"),
    ("<Ctrl>x", "edit.cut"),
    ("<Ctrl>c", "edit.copy"),
    ("<Ctrl>v", "edit.paste"),
    ("<Ctrl>z", "edit.undo"),
    ("<Ctrl>y", "edit.redo"),
    ("<Ctrl><Shift>z", "edit.redo"),
    ("<Ctrl>b", "win.export::pdf"),
    ("<Ctrl>p", "win.open-file"),
    ("<Ctrl>k <Ctrl>b", "win.toggle-sidebar"),
    ("<Ctrl>grave", "win.toggle-panel"),
    ("<Ctrl><Alt>Up", "cursor.add-above"),
    ("<Ctrl><Alt>Down", "cursor.add-below"),
];

const EMACS: &[(&str, &str)] = &[
    // motion (extends the region while the mark is active)
    ("<Ctrl>f", "move.right"),
    ("<Ctrl>b", "move.left"),
    ("<Ctrl>n", "move.down"),
    ("<Ctrl>p", "move.up"),
    ("<Ctrl>a", "move.line-start"),
    ("<Ctrl>e", "move.line-end"),
    ("<Alt>f", "move.word-right"),
    ("<Alt>b", "move.word-left"),
    ("<Ctrl>v", "move.page-down"),
    ("<Alt>v", "move.page-up"),
    ("<Alt><Shift>comma", "move.doc-start"),
    ("<Alt><Shift>period", "move.doc-end"),
    ("<Ctrl>l", "view.center"),
    ("<Alt>g g", "goto.line"),
    ("<Alt>g <Alt>g", "goto.line"),
    // editing
    ("<Ctrl>space", "mark.set"),
    ("<Ctrl>g", "mark.cancel"),
    ("<Ctrl>x h", "edit.select-all"),
    ("<Ctrl>x <Ctrl>x", "mark.exchange"),
    ("<Ctrl>d", "edit.delete-right"),
    ("<Alt>d", "edit.delete-word-right"),
    ("<Alt>BackSpace", "edit.delete-word-left"),
    ("<Ctrl>k", "edit.kill-line"),
    ("<Ctrl>w", "edit.cut"),
    ("<Alt>w", "edit.copy"),
    ("<Ctrl>y", "edit.paste"),
    ("<Ctrl>slash", "edit.undo"),
    ("<Ctrl><Shift>minus", "edit.undo"),
    ("<Ctrl>x u", "edit.undo"),
    ("<Ctrl><Shift>slash", "edit.redo"),
    ("<Ctrl><Alt>slash", "edit.redo"),
    ("<Ctrl>t", "edit.transpose"),
    ("<Ctrl>o", "line.open"),
    ("<Alt><Shift>6", "line.join-previous"),
    ("<Alt>semicolon", "comment.line"),
    ("<Ctrl>x <Ctrl>semicolon", "comment.line"),
    ("<Alt>u", "edit.upcase-word"),
    ("<Alt>l", "edit.downcase-word"),
    ("<Ctrl>x <Ctrl>u", "edit.upcase"),
    ("<Ctrl>x <Ctrl>l", "edit.downcase"),
    ("<Ctrl>x <Ctrl>t", "line.move-up"),
    // multiple-cursors.el
    ("<Ctrl><Shift>period", "cursor.add-next-match"),
    ("<Ctrl><Shift>comma", "cursor.undo"),
    ("<Ctrl>c <Ctrl><Shift>comma", "cursor.select-all-matches"),
    ("<Ctrl><Shift>c <Ctrl><Shift>c", "cursor.line-ends"),
    ("<Ctrl><Alt>Up", "cursor.add-above"),
    ("<Ctrl><Alt>Down", "cursor.add-below"),
    // files & windows
    ("<Ctrl>x <Ctrl>s", "win.save"),
    ("<Ctrl>x s", "win.save-all"),
    ("<Ctrl>x <Ctrl>f", "win.open-file"),
    ("<Ctrl>x d", "win.open-folder"),
    ("<Ctrl>x k", "win.close-tab"),
    ("<Ctrl>x b", "win.next-tab"),
    ("<Ctrl>x <Ctrl>c", "app.quit"),
    ("<Ctrl>x <Ctrl>e", "win.export::pdf"),
    ("<Ctrl>x p", "win.toggle-preview"),
    ("<Ctrl>s", "win.find"),
    ("<Ctrl>r", "win.find"),
    ("<Alt><Shift>5", "win.replace"),
    ("<Ctrl><Alt>backslash", "win.format"),
    ("<Alt>period", "win.goto-definition"),
];

/// Vim keys are handled by GtkSourceView's Vim emulation; only bindings that
/// Vim leaves free (Ctrl+Alt…) are added on top.
const VIM: &[(&str, &str)] = &[
    ("<Ctrl><Alt>Up", "cursor.add-above"),
    ("<Ctrl><Alt>Down", "cursor.add-below"),
    ("<Ctrl><Alt>d", "cursor.add-next-match"),
    ("<Ctrl><Alt>l", "cursor.select-all-matches"),
    ("<Ctrl>slash", "comment.line"),
];

pub const KEYMAPS: &[Keymap] = &[
    Keymap { id: "default", name: "Plars", add_cursor_click: gdk::ModifierType::ALT_MASK,
             box_drag: gdk::ModifierType::SHIFT_MASK.union(gdk::ModifierType::ALT_MASK),
             ctrl_click_definition: true, vim: false, bindings: DEFAULT },
    Keymap { id: "vscode", name: "VS Code", add_cursor_click: gdk::ModifierType::ALT_MASK,
             box_drag: gdk::ModifierType::SHIFT_MASK.union(gdk::ModifierType::ALT_MASK),
             ctrl_click_definition: true, vim: false, bindings: VSCODE },
    Keymap { id: "jetbrains", name: "JetBrains", add_cursor_click: gdk::ModifierType::ALT_MASK.union(gdk::ModifierType::SHIFT_MASK),
             box_drag: gdk::ModifierType::ALT_MASK,
             ctrl_click_definition: true, vim: false, bindings: JETBRAINS },
    Keymap { id: "sublime", name: "Sublime Text", add_cursor_click: gdk::ModifierType::CONTROL_MASK,
             box_drag: gdk::ModifierType::empty(),
             ctrl_click_definition: false, vim: false, bindings: SUBLIME },
    Keymap { id: "emacs", name: "Emacs", add_cursor_click: gdk::ModifierType::ALT_MASK,
             box_drag: gdk::ModifierType::empty(),
             ctrl_click_definition: true, vim: false, bindings: EMACS },
    Keymap { id: "vim", name: "Vim", add_cursor_click: gdk::ModifierType::ALT_MASK,
             box_drag: gdk::ModifierType::empty(),
             ctrl_click_definition: true, vim: true, bindings: VIM },
];

pub fn get(id: &str) -> &'static Keymap {
    KEYMAPS.iter().find(|k| k.id == id).unwrap_or(&KEYMAPS[0])
}

pub const MODS: gdk::ModifierType = gdk::ModifierType::CONTROL_MASK
    .union(gdk::ModifierType::SHIFT_MASK)
    .union(gdk::ModifierType::ALT_MASK)
    .union(gdk::ModifierType::SUPER_MASK);

fn parse_chord(s: &str) -> Chord {
    let (key, mods) = gtk::accelerator_parse(s).unwrap_or_else(|| panic!("bad accelerator {s}"));
    (key.to_lower(), mods & MODS)
}

fn parse_seq(s: &str) -> Vec<Chord> {
    s.split(' ').map(parse_chord).collect()
}

impl Keymap {
    pub fn lookup(&self, seq: &[Chord]) -> Lookup {
        let mut prefix = false;
        for (keys, cmd) in self.bindings {
            let b = parse_seq(keys);
            if b == seq {
                return Lookup::Command(cmd);
            }
            prefix |= b.len() > seq.len() && b[..seq.len()] == *seq;
        }
        if prefix { Lookup::Prefix } else { Lookup::None }
    }

    /// Bindings for display: (keys, command).
    pub fn bindings(&self) -> impl Iterator<Item = (String, &'static str)> + '_ {
        self.bindings.iter().map(|(k, c)| (k.split(' ').map(label_chord).collect::<Vec<_>>().join(" "), *c))
    }

    /// The full accelerator table for this keymap, ready for `set_accels_for_action`.
    pub fn accels(&self) -> Vec<(String, Vec<String>)> {
        let used: Vec<Chord> = self.bindings.iter().map(|(k, _)| parse_seq(k)[0]).collect();
        let mut out: Vec<(String, Vec<String>)> = BASE_ACCELS
            .iter()
            .map(|(action, accels)| {
                let keep = accels.iter().filter(|a| !used.contains(&parse_chord(a))).map(|a| a.to_string()).collect();
                (action.to_string(), keep)
            })
            .collect();
        for (keys, cmd) in self.bindings {
            if !(cmd.starts_with("win.") || cmd.starts_with("app.")) || keys.contains(' ') {
                continue;
            }
            match out.iter_mut().find(|(a, _)| a == cmd) {
                Some((_, list)) => list.push(keys.to_string()),
                None => out.push((cmd.to_string(), vec![keys.to_string()])),
            }
        }
        out
    }
}

pub fn label_chord(s: &str) -> String {
    let (key, mods) = gtk::accelerator_parse(s).unwrap_or((gdk::Key::VoidSymbol, gdk::ModifierType::empty()));
    gtk::accelerator_get_label(key, mods).to_string()
}

/// Normalise a key press into the chord form used by the tables. Shifted
/// symbols (e.g. `?`) are mapped back to their unshifted key (`slash`) so
/// bindings can always be written as base key + `<Shift>`.
pub fn event_chords(display: &gdk::Display, key: gdk::Key, code: u32, state: gdk::ModifierType) -> Vec<Chord> {
    let mods = state & MODS;
    let mut out = vec![(key.to_lower(), mods)];
    if mods.contains(gdk::ModifierType::SHIFT_MASK) {
        if let Some((base, ..)) = display.translate_key(code, state - gdk::ModifierType::SHIFT_MASK, 0) {
            let base = base.to_lower();
            if base != out[0].0 {
                out.push((base, mods));
            }
        }
    }
    out
}

pub fn is_modifier(key: gdk::Key) -> bool {
    use gdk::Key as K;
    matches!(key, K::Shift_L | K::Shift_R | K::Control_L | K::Control_R | K::Alt_L | K::Alt_R
        | K::Meta_L | K::Meta_R | K::Super_L | K::Super_R | K::ISO_Level3_Shift | K::Caps_Lock | K::Num_Lock)
}

/// Human description of a command, for the shortcuts window.
pub fn describe(cmd: &str) -> &'static str {
    match cmd {
        "cursor.add-next-match" => "Add caret at next occurrence",
        "cursor.skip-match" => "Skip occurrence (move last caret to next)",
        "cursor.select-all-matches" => "Add carets at all occurrences",
        "cursor.add-above" => "Add caret above (column)",
        "cursor.add-below" => "Add caret below (column)",
        "cursor.line-ends" => "Add carets at ends of selected lines",
        "cursor.split-lines" => "Split selection into lines",
        "cursor.undo" => "Remove last added caret",
        "line.duplicate" => "Duplicate line / selection",
        "line.delete" => "Delete line",
        "line.move-up" => "Move line up",
        "line.move-down" => "Move line down",
        "line.copy-up" => "Copy line up",
        "line.copy-down" => "Copy line down",
        "line.insert-below" => "Insert line below",
        "line.insert-above" => "Insert line above",
        "line.open" => "Open line (split after caret)",
        "line.join" => "Join lines",
        "line.join-previous" => "Join with previous line",
        "line.select" => "Select line",
        "comment.line" => "Toggle line comment",
        "comment.block" => "Toggle block comment",
        "edit.indent" => "Indent",
        "edit.outdent" => "Outdent",
        "edit.cut" => "Cut (line if nothing selected)",
        "edit.copy" => "Copy (line if nothing selected)",
        "edit.paste" => "Paste",
        "edit.undo" => "Undo",
        "edit.redo" => "Redo",
        "edit.select-all" => "Select all",
        "edit.delete-right" => "Delete character",
        "edit.delete-word-right" => "Delete word forward",
        "edit.delete-word-left" => "Delete word backward",
        "edit.kill-line" => "Kill to end of line",
        "edit.transpose" => "Transpose characters",
        "edit.upcase-word" => "Uppercase word",
        "edit.downcase-word" => "Lowercase word",
        "edit.upcase" => "Uppercase selection",
        "edit.downcase" => "Lowercase selection",
        "goto.bracket" => "Jump to matching bracket",
        "goto.line" => "Go to line",
        "select.expand" => "Expand selection",
        "select.shrink" => "Shrink selection",
        "move.right" => "Forward character",
        "move.left" => "Backward character",
        "move.down" => "Next line",
        "move.up" => "Previous line",
        "move.line-start" => "Beginning of line",
        "move.line-end" => "End of line",
        "move.word-right" => "Forward word",
        "move.word-left" => "Backward word",
        "move.page-down" => "Page down",
        "move.page-up" => "Page up",
        "move.doc-start" => "Beginning of buffer",
        "move.doc-end" => "End of buffer",
        "view.center" => "Recenter on caret",
        "mark.set" => "Set mark",
        "mark.cancel" => "Cancel (mark, carets, prefix)",
        "mark.exchange" => "Exchange caret and mark",
        "win.open-file" => "Open file",
        "win.open-folder" => "Open folder",
        "win.new-file" => "New file",
        "win.save" => "Save",
        "win.save-all" => "Save all",
        "win.close-tab" => "Close tab",
        "win.find" => "Find in file",
        "win.replace" => "Replace in file",
        "win.find-project" => "Find in project",
        "win.format" => "Format document",
        "win.goto-definition" => "Go to definition",
        "win.toggle-sidebar" => "Toggle sidebar",
        "win.toggle-panel" => "Toggle bottom panel",
        "win.toggle-preview" => "Toggle preview",
        "win.export::pdf" => "Export PDF",
        "win.docs" => "Typst documentation",
        "win.zoom-in" => "Zoom in",
        "win.zoom-out" => "Zoom out",
        "win.toggle-breakpoint" => "Toggle breakpoint",
        "win.debug-continue" => "Debug: start / continue",
        "win.debug-stop" => "Debug: stop",
        "win.debug-next" => "Debug: step over",
        "win.debug-in" => "Debug: step into",
        "win.debug-out" => "Debug: step out",
        "win.next-tab" => "Next tab",
        "win.prev-tab" => "Previous tab",
        "win.shortcuts" => "Keyboard shortcuts",
        "app.preferences" => "Preferences",
        "app.quit" => "Quit",
        "app.new-window" => "New window",
        _ => "",
    }
}
