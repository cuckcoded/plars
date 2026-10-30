//! Editor commands bound by the keymaps (see `keymap.rs`). Line commands
//! work on every caret: the lines each caret touches are merged into blocks
//! and processed bottom-up so earlier offsets stay valid.

use std::rc::Rc;

use gtk::{gdk, gio, glib};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::carets::{first_non_blank, is_word_char, line_end, line_start, Deletion, Motion};
use super::EditorPage;
use crate::keymap::{self, Lookup};

impl EditorPage {
    pub(super) fn keymap(&self) -> &'static keymap::Keymap {
        keymap::get(&self.settings.get().keymap)
    }

    /// Key presses on the view, in the capture phase (before GtkTextView's
    /// own bindings).
    pub(super) fn on_key(self: &Rc<Self>, key: gdk::Key, code: u32, state: gdk::ModifierType) -> glib::Propagation {
        if keymap::is_modifier(key) {
            return glib::Propagation::Proceed;
        }
        let km = self.keymap();
        let pending = self.key_seq.borrow().clone();
        let mut prefix = None;
        for chord in keymap::event_chords(&self.view.display(), key, code, state) {
            let mut seq = pending.clone();
            seq.push(chord);
            match km.lookup(&seq) {
                Lookup::Command(cmd) => {
                    self.set_key_seq(Vec::new());
                    self.run_command(cmd);
                    return glib::Propagation::Stop;
                }
                Lookup::Prefix => prefix = prefix.or(Some(seq)),
                Lookup::None => {}
            }
        }
        if let Some(seq) = prefix {
            self.set_key_seq(seq);
            return glib::Propagation::Stop;
        }
        if !pending.is_empty() {
            // unbound continuation of a prefix: swallow it, like Emacs
            self.set_key_seq(Vec::new());
            return glib::Propagation::Stop;
        }
        if self.has_extra_carets() && self.multi_caret_key(key, state) {
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }

    pub(super) fn set_key_seq(&self, seq: Vec<keymap::Chord>) {
        let label = seq
            .iter()
            .map(|(k, m)| gtk::accelerator_get_label(*k, *m).to_string())
            .collect::<Vec<_>>()
            .join(" ");
        if !self.keymap().vim {
            self.vim_label.set_label(&if label.is_empty() { label } else { format!("{label} –") });
        }
        // Application accelerators fire before the view sees a key, so they are
        // suspended while a sequence is pending (Emacs `C-x C-s` vs `C-s`).
        let was_pending = !self.key_seq.borrow().is_empty();
        let pending = !seq.is_empty();
        *self.key_seq.borrow_mut() = seq;
        if was_pending != pending {
            if let Some(app) = self.view.root().and_downcast::<gtk::Window>().and_then(|w| w.application()) {
                if pending {
                    for action in app.list_action_descriptions() {
                        app.set_accels_for_action(&action, &[]);
                    }
                } else {
                    crate::apply_accels(&app, &self.settings.get().keymap);
                }
            }
        }
    }

    /// Default handling of motion/deletion keys while there are extra carets.
    fn multi_caret_key(&self, key: gdk::Key, state: gdk::ModifierType) -> bool {
        use gdk::Key as K;
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let motion = match key {
            K::Left | K::KP_Left => Some(if ctrl { Motion::WordLeft } else { Motion::Left }),
            K::Right | K::KP_Right => Some(if ctrl { Motion::WordRight } else { Motion::Right }),
            K::Up | K::KP_Up if !ctrl => Some(Motion::Up),
            K::Down | K::KP_Down if !ctrl => Some(Motion::Down),
            K::Home | K::KP_Home => Some(if ctrl { Motion::DocStart } else { Motion::LineStart }),
            K::End | K::KP_End => Some(if ctrl { Motion::DocEnd } else { Motion::LineEnd }),
            K::Page_Up => Some(Motion::PageUp),
            K::Page_Down => Some(Motion::PageDown),
            _ => None,
        };
        if let Some(m) = motion {
            self.move_carets(m, shift || self.mark_active.get());
            return true;
        }
        match key {
            K::BackSpace => self.delete_at_carets(if ctrl { Deletion::WordLeft } else { Deletion::CharLeft }),
            K::Delete | K::KP_Delete => self.delete_at_carets(if ctrl { Deletion::WordRight } else { Deletion::CharRight }),
            K::Tab if !ctrl => self.run_editor_command(if shift { "edit.outdent" } else { "edit.indent" }),
            K::ISO_Left_Tab => self.run_editor_command("edit.outdent"),
            K::Escape => {
                // let Escape through too (completion, Vim insert mode)
                self.clear_carets();
                return false;
            }
            _ => return false,
        }
        true
    }

    pub fn run_command(self: &Rc<Self>, cmd: &str) {
        if cmd.starts_with("win.") || cmd.starts_with("app.") {
            if let Ok((name, target)) = gio::Action::parse_detailed_name(cmd) {
                let _ = self.view.activate_action(&name, target.as_ref());
            }
            return;
        }
        match cmd {
            "edit.paste" => self.paste(),
            _ => self.run_editor_command(cmd),
        }
    }

    fn run_editor_command(&self, cmd: &str) {
        let extend = self.mark_active.get();
        match cmd {
            // multiple carets
            "cursor.add-above" => self.add_caret_vertical(-1),
            "cursor.add-below" => self.add_caret_vertical(1),
            "cursor.add-next-match" => self.add_next_match(false),
            "cursor.skip-match" => self.add_next_match(true),
            "cursor.select-all-matches" => self.select_all_matches(),
            "cursor.line-ends" => self.split_selection(false),
            "cursor.split-lines" => self.split_selection(true),
            "cursor.undo" => self.pop_caret(),
            // lines
            "line.duplicate" => self.duplicate(),
            "line.copy-down" => self.copy_lines(true),
            "line.copy-up" => self.copy_lines(false),
            "line.delete" => self.delete_lines(),
            "line.move-up" => self.move_lines(false),
            "line.move-down" => self.move_lines(true),
            "line.insert-below" => self.insert_line(true),
            "line.insert-above" => self.insert_line(false),
            "line.open" => self.open_line(),
            "line.join" => self.join_lines(false),
            "line.join-previous" => self.join_lines(true),
            "line.select" => self.select_lines(),
            "comment.line" => self.toggle_line_comment(),
            "comment.block" => self.toggle_block_comment(),
            "edit.indent" => self.indent(true),
            "edit.outdent" => self.indent(false),
            // clipboard & history
            "edit.cut" => self.copy(true),
            "edit.copy" => self.copy(false),
            "edit.undo" | "edit.redo" => {
                // not inside a user action: GtkTextBuffer ignores undo there
                let was = self.replicating.replace(true);
                if cmd == "edit.undo" && self.buffer.can_undo() {
                    self.buffer.undo();
                } else if cmd == "edit.redo" && self.buffer.can_redo() {
                    self.buffer.redo();
                }
                self.replicating.set(was);
                self.refresh_carets();
            }
            "edit.select-all" => {
                self.clear_carets();
                let (s, e) = self.buffer.bounds();
                self.buffer.select_range(&e, &s);
            }
            "edit.delete-right" => self.delete_at_carets(Deletion::CharRight),
            "edit.delete-word-right" => self.delete_at_carets(Deletion::WordRight),
            "edit.delete-word-left" => self.delete_at_carets(Deletion::WordLeft),
            "edit.kill-line" => self.kill_line(),
            "edit.transpose" => self.transpose(),
            "edit.upcase-word" => self.change_case(true, true),
            "edit.downcase-word" => self.change_case(false, true),
            "edit.upcase" => self.change_case(true, false),
            "edit.downcase" => self.change_case(false, false),
            // navigation
            "goto.bracket" => self.goto_bracket(),
            "goto.line" => self.show_goto_line(),
            "select.expand" => self.expand_selection(),
            "select.shrink" => self.shrink_selection(),
            "move.left" => self.move_carets(Motion::Left, extend),
            "move.right" => self.move_carets(Motion::Right, extend),
            "move.up" => self.move_carets(Motion::Up, extend),
            "move.down" => self.move_carets(Motion::Down, extend),
            "move.line-start" => self.move_carets(Motion::LineStart, extend),
            "move.line-end" => self.move_carets(Motion::LineEnd, extend),
            "move.word-left" => self.move_carets(Motion::WordLeft, extend),
            "move.word-right" => self.move_carets(Motion::WordRight, extend),
            "move.page-up" => self.move_carets(Motion::PageUp, extend),
            "move.page-down" => self.move_carets(Motion::PageDown, extend),
            "move.doc-start" => self.move_carets(Motion::DocStart, extend),
            "move.doc-end" => self.move_carets(Motion::DocEnd, extend),
            "view.center" => self.view.scroll_to_mark(&self.buffer.get_insert(), 0.0, true, 0.0, 0.5),
            // Emacs mark
            "mark.set" => {
                let it = self.cursor();
                self.buffer.select_range(&it, &it);
                self.mark_active.set(true);
                self.vim_label.set_label("Mark set");
            }
            "mark.cancel" => {
                self.mark_active.set(false);
                self.clear_carets();
                let it = self.cursor();
                self.buffer.select_range(&it, &it);
                self.vim_label.set_label("Quit");
            }
            "mark.exchange" => {
                let ins = self.cursor();
                let bound = self.buffer.iter_at_mark(&self.buffer.selection_bound());
                self.buffer.select_range(&bound, &ins);
                self.mark_active.set(true);
            }
            _ => eprintln!("plars: unknown editor command {cmd}"),
        }
    }

    // -- carets -----------------------------------------------------------

    fn add_caret_vertical(&self, dir: i32) {
        let carets = self.carets_sorted();
        let edge = if dir < 0 { carets.first() } else { carets.last() };
        let Some(edge) = edge else { return };
        let it = self.buffer.iter_at_mark(&edge.ins);
        let col = self.cursor().line_offset();
        let line = it.line() + dir;
        if line < 0 || line >= self.buffer.line_count() {
            return;
        }
        let target = super::carets::iter_at_line_col(self.buffer.upcast_ref(), line, col);
        self.add_caret(&target, &target);
        let m = self.buffer.create_mark(None, &target, false);
        self.view.scroll_mark_onscreen(&m);
        self.buffer.delete_mark(&m);
    }

    /// Word under the primary caret.
    fn word_at_cursor(&self) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let it = self.cursor();
        let (mut s, mut e) = (it, it);
        loop {
            let mut p = s;
            if !p.backward_char() || !is_word_char(p.char()) {
                break;
            }
            s = p;
        }
        while !e.is_end() && is_word_char(e.char()) {
            e.forward_char();
        }
        (s != e).then_some((s, e))
    }

    /// The primary selection; selects the word under the caret if empty.
    fn needle(&self) -> Option<String> {
        let (s, e) = self.buffer.selection_bounds().unwrap_or_else(|| (self.cursor(), self.cursor()));
        if s == e {
            let (s, e) = self.word_at_cursor()?;
            self.buffer.select_range(&e, &s);
            return None;
        }
        Some(self.buffer.text(&s, &e, true).to_string())
    }

    fn find_next(&self, needle: &str, from: &gtk::TextIter) -> Option<(gtk::TextIter, gtk::TextIter)> {
        let flags = gtk::TextSearchFlags::TEXT_ONLY;
        let taken: Vec<i32> = self.carets().iter().map(|c| self.range_of(c).0.offset()).collect();
        let mut at = *from;
        let mut wrapped = false;
        loop {
            match at.forward_search(needle, flags, None) {
                Some((s, e)) if taken.contains(&s.offset()) => {
                    if wrapped && s.offset() >= from.offset() {
                        return None;
                    }
                    at = e;
                }
                Some(found) => return Some(found),
                None if !wrapped => {
                    wrapped = true;
                    at = self.buffer.start_iter();
                }
                None => return None,
            }
        }
    }

    fn add_next_match(&self, skip: bool) {
        let Some(needle) = self.needle() else { return };
        let last = self.extras().last().cloned();
        let from = match &last {
            Some(c) => self.range_of(c).1,
            None => self.buffer.selection_bounds().map(|(_, e)| e).unwrap_or_else(|| self.cursor()),
        };
        if skip {
            match last {
                Some(_) => self.pop_caret(),
                None => {
                    if let Some((s, e)) = self.find_next(&needle, &from) {
                        self.buffer.select_range(&e, &s);
                        self.view.scroll_mark_onscreen(&self.buffer.get_insert());
                    }
                    return;
                }
            }
        }
        if let Some((s, e)) = self.find_next(&needle, &from) {
            self.add_caret(&e, &s);
            let m = self.buffer.create_mark(None, &e, false);
            self.view.scroll_mark_onscreen(&m);
            self.buffer.delete_mark(&m);
        }
    }

    fn select_all_matches(&self) {
        let needle = match self.needle() {
            Some(n) => n,
            None => match self.needle() {
                Some(n) => n,
                None => return,
            },
        };
        while let Some((s, e)) = self.find_next(&needle, &self.buffer.start_iter()) {
            self.add_caret(&e, &s);
        }
    }

    /// One caret per line of the primary selection: at the line end, or
    /// (`keep_selection`) selecting that line's part of the selection.
    fn split_selection(&self, keep_selection: bool) {
        let Some((s, e)) = self.buffer.selection_bounds() else { return };
        self.clear_carets();
        let (first, mut last) = (s.line(), e.line());
        if last > first && e.starts_line() {
            last -= 1;
        }
        let buf = self.buffer.upcast_ref::<gtk::TextBuffer>();
        let mut ranges = Vec::new();
        for line in first..=last {
            let ls = if line == first { s } else { line_start(buf, line) };
            let le = if line == e.line() { e } else { line_end(&line_start(buf, line)) };
            ranges.push(if keep_selection { (ls, le) } else { (le, le) });
        }
        let (ps, pe) = ranges.pop().unwrap();
        self.buffer.select_range(&pe, &ps);
        for (a, b) in ranges {
            let (a, b) = (self.buffer.iter_at_offset(a.offset()), self.buffer.iter_at_offset(b.offset()));
            self.add_caret(&b, &a);
        }
    }

    // -- line blocks --------------------------------------------------------

    /// (first, last) line of every caret, merged, top to bottom.
    fn line_blocks(&self) -> Vec<(i32, i32)> {
        let mut blocks: Vec<(i32, i32)> = self
            .carets()
            .iter()
            .map(|c| {
                let (s, e) = self.range_of(c);
                let last = if e.line() > s.line() && e.starts_line() { e.line() - 1 } else { e.line() };
                (s.line(), last)
            })
            .collect();
        blocks.sort();
        let mut merged: Vec<(i32, i32)> = Vec::new();
        for b in blocks {
            match merged.last_mut() {
                Some(m) if b.0 <= m.1 => m.1 = m.1.max(b.1),
                _ => merged.push(b),
            }
        }
        merged
    }

    fn buf(&self) -> &gtk::TextBuffer {
        self.buffer.upcast_ref()
    }

    /// Text of lines first..=last, always ending in a newline.
    fn block_text(&self, first: i32, last: i32) -> String {
        let s = line_start(self.buf(), first);
        let e = line_end(&line_start(self.buf(), last));
        format!("{}\n", self.buffer.text(&s, &e, true))
    }

    fn duplicate(&self) {
        // a selection inside one line duplicates just the selection
        if !self.has_extra_carets() {
            if let Some((s, e)) = self.buffer.selection_bounds() {
                if s.line() == e.line() {
                    let text = self.buffer.text(&s, &e, true).to_string();
                    self.edit(|| {
                        let mut at = self.buffer.selection_bounds().map(|(_, e)| e).unwrap_or_else(|| self.cursor());
                        self.buffer.insert(&mut at, &text);
                    });
                    return;
                }
            }
        }
        self.copy_lines(true);
    }

    /// Copy each block below (carets follow the copy) or above it.
    fn copy_lines(&self, down: bool) {
        self.edit(|| {
            for (first, last) in self.line_blocks().into_iter().rev() {
                let text = self.block_text(first, last);
                if down {
                    // inserting before the block pushes its carets onto the lower copy
                    let mut at = line_start(self.buf(), first);
                    self.buffer.insert(&mut at, &text);
                } else {
                    let mut at = line_end(&line_start(self.buf(), last));
                    self.buffer.insert(&mut at, &format!("\n{}", text.trim_end_matches('\n')));
                    // carets stay on the upper copy
                }
            }
        });
    }

    fn delete_lines(&self) {
        self.edit(|| {
            for (first, last) in self.line_blocks().into_iter().rev() {
                let mut s = line_start(self.buf(), first);
                let mut e = line_start(self.buf(), last + 1);
                if last + 1 >= self.buffer.line_count() && first > 0 {
                    // last line has no newline: take the one before it instead
                    s = line_end(&line_start(self.buf(), first - 1));
                    e = self.buffer.end_iter();
                }
                self.buffer.delete(&mut s, &mut e);
            }
        });
    }

    fn move_lines(&self, down: bool) {
        let blocks = self.line_blocks();
        let n = self.buffer.line_count();
        if blocks.iter().any(|&(f, l)| if down { l + 1 >= n } else { f == 0 }) {
            return;
        }
        self.edit(|| {
            let order: Vec<_> = if down { blocks.into_iter().rev().collect() } else { blocks };
            for (first, last) in order {
                if down {
                    // take the line below and put it above the block
                    let below = last + 1;
                    let at_end = below + 1 >= self.buffer.line_count();
                    let ls = line_start(self.buf(), below);
                    let text = self.buffer.text(&ls, &line_end(&ls), true).to_string();
                    let (mut s, mut e) = if at_end {
                        (line_end(&line_start(self.buf(), last)), self.buffer.end_iter())
                    } else {
                        (ls, line_start(self.buf(), below + 1))
                    };
                    self.buffer.delete(&mut s, &mut e);
                    let mut at = line_start(self.buf(), first);
                    self.buffer.insert(&mut at, &format!("{text}\n"));
                } else {
                    // take the line above and put it below the block
                    let above = first - 1;
                    let ls = line_start(self.buf(), above);
                    let text = self.buffer.text(&ls, &line_end(&ls), true).to_string();
                    let (mut s, mut e) = (ls, line_start(self.buf(), first));
                    self.buffer.delete(&mut s, &mut e);
                    let last = last - 1;
                    if last + 1 >= self.buffer.line_count() {
                        let mut at = self.buffer.end_iter();
                        self.buffer.insert(&mut at, &format!("\n{text}"));
                    } else {
                        let mut at = line_start(self.buf(), last + 1);
                        self.buffer.insert(&mut at, &format!("{text}\n"));
                    }
                }
            }
        });
        self.view.scroll_mark_onscreen(&self.buffer.get_insert());
    }

    fn leading_ws(&self, it: &gtk::TextIter) -> String {
        let mut s = *it;
        s.set_line_offset(0);
        self.buffer.text(&s, &first_non_blank(it), true).to_string()
    }

    fn insert_line(&self, below: bool) {
        self.edit(|| {
            for c in self.carets() {
                let it = self.buffer.iter_at_mark(&c.ins);
                let indent = self.leading_ws(&it);
                if below {
                    let mut at = line_end(&it);
                    self.buffer.insert(&mut at, &format!("\n{indent}"));
                    self.set_caret(&c, &at, &at);
                } else {
                    let mut at = it;
                    at.set_line_offset(0);
                    let off = at.offset();
                    self.buffer.insert(&mut at, &format!("{indent}\n"));
                    let pos = self.buffer.iter_at_offset(off + indent.chars().count() as i32);
                    self.set_caret(&c, &pos, &pos);
                }
            }
        });
    }

    fn open_line(&self) {
        self.edit(|| {
            for c in self.carets() {
                let mut at = self.buffer.iter_at_mark(&c.ins);
                let off = at.offset();
                self.buffer.insert(&mut at, "\n");
                let pos = self.buffer.iter_at_offset(off);
                self.set_caret(&c, &pos, &pos);
            }
        });
    }

    /// Join each block's lines (a one-line block joins with the next line).
    fn join_lines(&self, with_previous: bool) {
        self.edit(|| {
            for (first, last) in self.line_blocks().into_iter().rev() {
                let (first, last) = if with_previous { (first - 1, first) } else { (first, last.max(first + 1)) };
                if first < 0 || last >= self.buffer.line_count() {
                    continue;
                }
                for line in (first..last).rev() {
                    let mut s = line_end(&line_start(self.buf(), line));
                    let mut e = first_non_blank(&line_start(self.buf(), line + 1));
                    let trailing = {
                        let mut p = s;
                        p.backward_char() && p.char() == ' '
                    };
                    let sep = if e.ends_line() || trailing || s.starts_line() { "" } else { " " };
                    let off = s.offset();
                    self.buffer.delete(&mut s, &mut e);
                    let mut at = self.buffer.iter_at_offset(off);
                    self.buffer.insert(&mut at, sep);
                    if with_previous {
                        let pos = self.buffer.iter_at_offset(off);
                        self.buffer.place_cursor(&pos);
                    }
                }
            }
        });
    }

    fn select_lines(&self) {
        for c in self.carets() {
            let (s, e) = self.range_of(&c);
            let start = line_start(self.buf(), s.line());
            let end = line_start(self.buf(), e.line() + 1);
            self.set_caret(&c, &end, &start);
        }
        self.refresh_carets();
    }

    // -- comments & indentation -------------------------------------------

    fn comment_tokens(&self) -> (Option<String>, Option<(String, String)>) {
        let lang = sourceview5::prelude::BufferExt::language(&self.buffer);
        let meta = |k: &str| lang.as_ref().and_then(|l| l.metadata(k)).map(|s| s.to_string());
        let id = lang.as_ref().map(|l| l.id().to_string()).unwrap_or_default();
        let line = meta("line-comment-start").or_else(|| match id.as_str() {
            "typst" | "c" | "cpp" | "rust" | "js" | "json" => Some("//".into()),
            "python" | "python3" | "sh" | "toml" | "yaml" | "makefile" => Some("#".into()),
            "latex" => Some("%".into()),
            _ if self.is_typst() => Some("//".into()),
            _ => None,
        });
        let block = match (meta("block-comment-start"), meta("block-comment-end")) {
            (Some(s), Some(e)) => Some((s, e)),
            _ if line.as_deref() == Some("//") => Some(("/*".into(), "*/".into())),
            _ => None,
        };
        (line, block)
    }

    fn toggle_line_comment(&self) {
        let (Some(tok), _) = self.comment_tokens() else { return };
        self.edit(|| {
            for (first, last) in self.line_blocks().into_iter().rev() {
                let lines: Vec<String> = (first..=last)
                    .map(|l| {
                        let s = line_start(self.buf(), l);
                        self.buffer.text(&s, &line_end(&s), true).to_string()
                    })
                    .collect();
                let filled: Vec<&String> = lines.iter().filter(|l| !l.trim().is_empty()).collect();
                if filled.is_empty() {
                    continue;
                }
                let uncomment = filled.iter().all(|l| l.trim_start().starts_with(&tok));
                let indent = filled.iter().map(|l| l.chars().take_while(|c| c.is_whitespace()).count()).min().unwrap_or(0);
                for (i, text) in lines.iter().enumerate().rev() {
                    if text.trim().is_empty() {
                        continue;
                    }
                    let ls = line_start(self.buf(), first + i as i32);
                    if uncomment {
                        let ws = text.chars().take_while(|c| c.is_whitespace()).count() as i32;
                        let mut n = tok.chars().count() as i32;
                        if text.chars().nth((ws + n) as usize) == Some(' ') {
                            n += 1;
                        }
                        let mut s = self.buffer.iter_at_offset(ls.offset() + ws);
                        let mut e = self.buffer.iter_at_offset(ls.offset() + ws + n);
                        self.buffer.delete(&mut s, &mut e);
                    } else {
                        let mut at = self.buffer.iter_at_offset(ls.offset() + indent as i32);
                        self.buffer.insert(&mut at, &format!("{tok} "));
                    }
                }
            }
        });
    }

    fn toggle_block_comment(&self) {
        let (_, Some((open, close))) = self.comment_tokens() else { return };
        self.edit(|| {
            for c in self.carets_sorted().into_iter().rev() {
                let (s, e) = self.range_of(&c);
                let (s, e) = if s == e { (first_non_blank(&s), line_end(&s)) } else { (s, e) };
                let text = self.buffer.text(&s, &e, true).to_string();
                let (so, eo) = (s.offset(), e.offset());
                let t = text.trim();
                if t.starts_with(&open) && t.ends_with(&close) && t.len() >= open.len() + close.len() {
                    let inner = t[open.len()..t.len() - close.len()].trim().to_string();
                    let lead = text.len() - text.trim_start().len();
                    let trail = text.trim_end().len();
                    let new = format!("{}{}{}", &text[..lead], inner, &text[trail..]);
                    let (mut a, mut b) = (s, e);
                    self.buffer.delete(&mut a, &mut b);
                    let mut at = self.buffer.iter_at_offset(so);
                    self.buffer.insert(&mut at, &new);
                } else {
                    let mut at = self.buffer.iter_at_offset(eo);
                    self.buffer.insert(&mut at, &format!(" {close}"));
                    let mut at = self.buffer.iter_at_offset(so);
                    self.buffer.insert(&mut at, &format!("{open} "));
                }
            }
        });
    }

    fn indent(&self, more: bool) {
        let width = self.settings.get().tab_width as usize;
        let unit = " ".repeat(width);
        // a single caret without a multi-line selection just inserts an indent
        let multiline = self.carets().iter().any(|c| {
            let (s, e) = self.range_of(c);
            s.line() != e.line()
        });
        if more && !multiline && self.has_extra_carets() {
            self.insert_at_carets(&[unit]);
            return;
        }
        self.edit(|| {
            for (first, last) in self.line_blocks().into_iter().rev() {
                for line in (first..=last).rev() {
                    let ls = line_start(self.buf(), line);
                    if more {
                        if !ls.ends_line() {
                            let mut at = ls;
                            self.buffer.insert(&mut at, &unit);
                        }
                    } else {
                        let ws = self.leading_ws(&ls);
                        let n = if ws.starts_with('\t') { 1 } else { ws.chars().take(width).take_while(|c| *c == ' ').count() };
                        let mut s = ls;
                        let mut e = self.buffer.iter_at_offset(ls.offset() + n as i32);
                        self.buffer.delete(&mut s, &mut e);
                    }
                }
            }
        });
    }

    // -- clipboard ----------------------------------------------------------

    /// Copy (or cut) every selection, or whole lines when nothing is selected.
    fn copy(&self, cut: bool) {
        let carets = self.carets_sorted();
        let any_sel = carets.iter().any(|c| {
            let (s, e) = self.range_of(c);
            s != e
        });
        let mut parts = Vec::new();
        let mut ranges = Vec::new();
        if any_sel {
            for c in &carets {
                let (s, e) = self.range_of(c);
                if s != e {
                    parts.push(self.buffer.text(&s, &e, true).to_string());
                    ranges.push((s.offset(), e.offset()));
                }
            }
        } else {
            for (first, last) in self.line_blocks() {
                parts.push(self.block_text(first, last));
                let e = line_start(self.buf(), last + 1);
                ranges.push((line_start(self.buf(), first).offset(), e.offset()));
            }
        }
        let text = if any_sel { parts.join("\n") } else { parts.concat() };
        self.view.clipboard().set_text(&text);
        self.mark_active.set(false);
        if cut {
            self.edit(|| {
                for (s, e) in ranges.into_iter().rev() {
                    let (mut s, mut e) = (self.buffer.iter_at_offset(s), self.buffer.iter_at_offset(e));
                    self.buffer.delete(&mut s, &mut e);
                }
            });
        } else if self.keymap().id == "emacs" {
            let it = self.cursor();
            self.buffer.select_range(&it, &it);
        }
    }

    fn paste(self: &Rc<Self>) {
        self.mark_active.set(false);
        if !self.has_extra_carets() {
            self.view.emit_paste_clipboard();
            return;
        }
        let w = Rc::downgrade(self);
        let clip = self.view.clipboard();
        glib::spawn_future_local(async move {
            let Ok(Some(text)) = clip.read_text_future().await else { return };
            let Some(p) = w.upgrade() else { return };
            let n = p.carets().len();
            let lines: Vec<String> = text.trim_end_matches('\n').split('\n').map(str::to_owned).collect();
            if lines.len() == n {
                p.insert_at_carets(&lines);
            } else {
                p.insert_at_carets(&[text.to_string()]);
            }
        });
    }

    fn kill_line(&self) {
        let mut killed = String::new();
        self.edit(|| {
            for c in self.carets() {
                let mut s = self.buffer.iter_at_mark(&c.ins);
                let mut e = if s.ends_line() { let mut n = s; n.forward_char(); n } else { line_end(&s) };
                if c == self.carets()[0] {
                    killed = self.buffer.text(&s, &e, true).to_string();
                }
                self.buffer.delete(&mut s, &mut e);
            }
        });
        if !killed.is_empty() {
            self.view.clipboard().set_text(&killed);
        }
    }

    fn transpose(&self) {
        self.edit(|| {
            for c in self.carets() {
                let mut it = self.buffer.iter_at_mark(&c.ins);
                if it.ends_line() && !it.starts_line() {
                    it.backward_char();
                }
                let mut a = it;
                if !a.backward_char() || it.ends_line() {
                    continue;
                }
                let mut b = it;
                b.forward_char();
                let (x, y) = (a.char(), it.char());
                let off = a.offset();
                self.buffer.delete(&mut a, &mut b);
                let mut at = self.buffer.iter_at_offset(off);
                self.buffer.insert(&mut at, &format!("{y}{x}"));
                let pos = self.buffer.iter_at_offset(off + 2);
                self.set_caret(&c, &pos, &pos);
            }
        });
    }

    fn change_case(&self, upper: bool, word: bool) {
        self.edit(|| {
            for c in self.carets() {
                let (s, e) = self.range_of(&c);
                let (s, e) = if word || s == e {
                    let mut e = s;
                    e.forward_word_end();
                    (s, e)
                } else {
                    (s, e)
                };
                let text = self.buffer.text(&s, &e, true).to_string();
                let new = if upper { text.to_uppercase() } else { text.to_lowercase() };
                let (so, keep_sel) = (s.offset(), !word && self.range_of(&c).0 != self.range_of(&c).1);
                let (mut a, mut b) = (s, e);
                self.buffer.delete(&mut a, &mut b);
                let mut at = self.buffer.iter_at_offset(so);
                self.buffer.insert(&mut at, &new);
                let end = self.buffer.iter_at_offset(so + new.chars().count() as i32);
                let start = if keep_sel { self.buffer.iter_at_offset(so) } else { end };
                self.set_caret(&c, &end, &start);
            }
        });
    }

    // -- brackets, selection, go to line ------------------------------------

    fn chars(&self) -> Vec<char> {
        self.text().chars().collect()
    }

    fn goto_bracket(&self) {
        let chars = self.chars();
        let pos = self.cursor().offset() as usize;
        let at = |i: usize| chars.get(i).copied().filter(|c| "()[]{}".contains(*c));
        let Some(i) = at(pos).map(|_| pos).or_else(|| pos.checked_sub(1).filter(|&p| at(p).is_some())) else { return };
        if let Some(j) = matching_bracket(&chars, i) {
            let it = self.buffer.iter_at_offset(j as i32);
            self.clear_carets();
            self.buffer.place_cursor(&it);
            self.view.scroll_mark_onscreen(&self.buffer.get_insert());
        }
    }

    fn expand_selection(&self) {
        self.clear_carets();
        let (s, e) = self.buffer.selection_bounds().unwrap_or_else(|| (self.cursor(), self.cursor()));
        let cur = (s.offset(), e.offset());
        if self.expand_stack.borrow().last().map(|r| r.1) != Some(cur) {
            self.expand_stack.borrow_mut().clear();
        }
        let chars = self.chars();
        let line = |it: &gtk::TextIter| {
            let fnb = first_non_blank(it).offset();
            (fnb, line_end(it).offset())
        };
        let mut candidates: Vec<(i32, i32)> = Vec::new();
        if s == e {
            if let Some((a, b)) = self.word_at_cursor() {
                candidates.push((a.offset(), b.offset()));
            }
        }
        candidates.extend(enclosing_pairs(&chars, cur.0 as usize, cur.1 as usize).into_iter().map(|(a, b)| (a as i32, b as i32)));
        if s.line() == e.line() {
            candidates.push(line(&s));
            let full_end = line_start(self.buf(), e.line() + 1).offset();
            candidates.push((line_start(self.buf(), s.line()).offset(), full_end));
        }
        candidates.push((0, chars.len() as i32));
        let next = candidates
            .into_iter()
            .filter(|&(a, b)| a <= cur.0 && b >= cur.1 && (a, b) != cur)
            .min_by_key(|&(a, b)| b - a);
        if let Some((a, b)) = next {
            self.expand_stack.borrow_mut().push((cur, (a, b)));
            self.buffer.select_range(&self.buffer.iter_at_offset(b), &self.buffer.iter_at_offset(a));
        }
    }

    fn shrink_selection(&self) {
        let popped = self.expand_stack.borrow_mut().pop();
        if let Some(((a, b), _)) = popped {
            self.buffer.select_range(&self.buffer.iter_at_offset(b), &self.buffer.iter_at_offset(a));
        }
    }

    fn show_goto_line(&self) {
        let existing = self.pview().imp().goto_popover.borrow().clone();
        let pop = existing.unwrap_or_else(|| {
            let entry = gtk::Entry::builder().placeholder_text("Line[:Column]").width_chars(14).build();
            let pop = gtk::Popover::builder().child(&entry).position(gtk::PositionType::Bottom).build();
            pop.set_parent(&self.view);
            let (buf, view, p) = (self.buffer.clone(), self.view.clone(), pop.clone());
            entry.connect_activate(move |e| {
                let text = e.text();
                let mut parts = text.split(':').map(|s| s.trim().parse::<i32>().ok());
                if let Some(Some(line)) = parts.next() {
                    let col = parts.next().flatten().unwrap_or(1);
                    let it = super::carets::iter_at_line_col(buf.upcast_ref(), line - 1, col - 1);
                    buf.place_cursor(&it);
                    view.scroll_to_mark(&buf.get_insert(), 0.0, true, 0.0, 0.3);
                }
                p.popdown();
                view.grab_focus();
            });
            *self.pview().imp().goto_popover.borrow_mut() = Some(pop.clone());
            pop
        });
        self.clear_carets();
        let r = self.view.iter_location(&self.cursor());
        let (x, y) = self.view.buffer_to_window_coords(gtk::TextWindowType::Widget, r.x(), r.y() + r.height());
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x, y, 1, 1)));
        if let Some(entry) = pop.child().and_downcast::<gtk::Entry>() {
            entry.set_text(&format!("{}", self.cursor().line() + 1));
            entry.select_region(0, -1);
        }
        pop.popup();
    }
}

fn matching_bracket(chars: &[char], i: usize) -> Option<usize> {
    let (open, close, fwd) = match chars[i] {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        _ => return None,
    };
    let mut depth = 0i32;
    let idx: Box<dyn Iterator<Item = usize>> = if fwd { Box::new(i..chars.len()) } else { Box::new((0..=i).rev()) };
    for j in idx {
        let c = chars[j];
        if c == open {
            depth += if fwd { 1 } else { -1 };
        } else if c == close {
            depth += if fwd { -1 } else { 1 };
        }
        if depth == 0 {
            return Some(j);
        }
    }
    None
}

/// Ranges enclosing [s, e): bracket/quote contents and the same with delimiters.
fn enclosing_pairs(chars: &[char], s: usize, e: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    // brackets: walk left for an unmatched opener
    let mut depth = 0i32;
    let mut i = s;
    while i > 0 {
        i -= 1;
        match chars[i] {
            ')' | ']' | '}' => depth += 1,
            '(' | '[' | '{' => {
                if depth == 0 {
                    if let Some(j) = matching_bracket(chars, i) {
                        if j >= e {
                            out.push((i + 1, j));
                            out.push((i, j + 1));
                        }
                    }
                } else {
                    depth -= 1;
                }
            }
            _ => {}
        }
    }
    // quotes and math on the same line
    for q in ['"', '$'] {
        let ls = chars[..s].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1);
        let le = chars[e.min(chars.len())..].iter().position(|c| *c == '\n').map_or(chars.len(), |p| p + e);
        let before = chars[ls..s].iter().filter(|c| **c == q).count();
        if before % 2 == 1 {
            let a = ls + chars[ls..s].iter().rposition(|c| *c == q).unwrap();
            if let Some(b) = chars[e..le].iter().position(|c| *c == q).map(|p| p + e) {
                out.push((a + 1, b));
                out.push((a, b + 1));
            }
        }
    }
    out
}
