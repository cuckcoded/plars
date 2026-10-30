//! Multiple carets. GtkSourceView has a single cursor, so extra carets are
//! pairs of buffer marks (insert + selection bound) kept on a `PlarsView`
//! subclass, which draws them. Typing at the primary caret is replicated at
//! every extra caret from the buffer's insert-text / delete-range signals;
//! motion and deletion keys are handled for all carets explicitly.

use std::cell::RefCell;

use gtk::glib::subclass::types::ObjectSubclassIsExt;
use gtk::{glib, graphene};
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use sourceview5::subclass::prelude::*;

use super::EditorPage;

#[derive(Clone, PartialEq)]
pub struct Caret {
    pub ins: gtk::TextMark,
    pub bound: gtk::TextMark,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PlarsView {
        pub extras: RefCell<Vec<Caret>>,
        pub goto_popover: RefCell<Option<gtk::Popover>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlarsView {
        const NAME: &'static str = "PlarsView";
        type Type = super::PlarsView;
        type ParentType = sourceview5::View;
    }

    impl ObjectImpl for PlarsView {
        fn dispose(&self) {
            if let Some(p) = self.goto_popover.take() {
                p.unparent();
            }
        }
    }
    impl WidgetImpl for PlarsView {}
    impl ViewImpl for PlarsView {}

    impl TextViewImpl for PlarsView {
        fn snapshot_layer(&self, layer: gtk::TextViewLayer, snapshot: gtk::Snapshot) {
            self.parent_snapshot_layer(layer, snapshot.clone());
            if layer != gtk::TextViewLayer::AboveText {
                return;
            }
            let view = self.obj();
            let buf = view.buffer();
            let color = view.color();
            for c in self.extras.borrow().iter() {
                let r = view.iter_location(&buf.iter_at_mark(&c.ins));
                snapshot.append_color(&color, &graphene::Rect::new(r.x() as f32, r.y() as f32, 2.0, r.height() as f32));
            }
        }
    }
}

glib::wrapper! {
    pub struct PlarsView(ObjectSubclass<imp::PlarsView>)
        @extends sourceview5::View, gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

#[derive(Clone, Copy, PartialEq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    WordLeft,
    WordRight,
    PageUp,
    PageDown,
    DocStart,
    DocEnd,
}

#[derive(Clone, Copy)]
pub enum Deletion {
    CharLeft,
    CharRight,
    WordLeft,
    WordRight,
    LineEnd,
}

pub enum Replay {
    Insert(String),
    Delete(PendingDelete),
}

/// What the primary caret's pending deletion looked like, to replay it elsewhere.
#[derive(Clone, Copy)]
pub enum PendingDelete {
    Selection,
    Around(i32, i32),
}

pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Iterator at (line, column), clamping the column to the line's length.
pub fn iter_at_line_col(buf: &gtk::TextBuffer, line: i32, col: i32) -> gtk::TextIter {
    let Some(mut it) = buf.iter_at_line(line.clamp(0, buf.line_count() - 1)) else { return buf.end_iter() };
    let mut end = it;
    if !end.ends_line() {
        end.forward_to_line_end();
    }
    it.set_line_offset(col.clamp(0, end.line_offset()));
    it
}

pub fn line_start(buf: &gtk::TextBuffer, line: i32) -> gtk::TextIter {
    if line >= buf.line_count() {
        return buf.end_iter();
    }
    buf.iter_at_line(line).unwrap_or_else(|| buf.end_iter())
}

pub fn line_end(it: &gtk::TextIter) -> gtk::TextIter {
    let mut e = *it;
    if !e.ends_line() {
        e.forward_to_line_end();
    }
    e
}

/// First non-blank character on the iterator's line.
pub fn first_non_blank(it: &gtk::TextIter) -> gtk::TextIter {
    let mut s = *it;
    s.set_line_offset(0);
    while !s.ends_line() && s.char().is_whitespace() {
        s.forward_char();
    }
    s
}

impl EditorPage {
    pub(super) fn pview(&self) -> &PlarsView {
        self.view.downcast_ref::<PlarsView>().expect("editor view is a PlarsView")
    }

    pub fn has_extra_carets(&self) -> bool {
        !self.pview().imp().extras.borrow().is_empty()
    }

    pub(super) fn extras(&self) -> Vec<Caret> {
        self.pview().imp().extras.borrow().clone()
    }

    fn primary(&self) -> Caret {
        Caret { ins: self.buffer.get_insert(), bound: self.buffer.selection_bound() }
    }

    /// Primary caret first, then the extra ones in the order they were added.
    pub(super) fn carets(&self) -> Vec<Caret> {
        let mut v = vec![self.primary()];
        v.extend(self.extras());
        v
    }

    /// All carets sorted by position in the buffer.
    pub(super) fn carets_sorted(&self) -> Vec<Caret> {
        let mut v = self.carets();
        v.sort_by_key(|c| self.range_of(c).0.offset());
        v
    }

    pub(super) fn range_of(&self, c: &Caret) -> (gtk::TextIter, gtk::TextIter) {
        let a = self.buffer.iter_at_mark(&c.ins);
        let b = self.buffer.iter_at_mark(&c.bound);
        if a.offset() <= b.offset() { (a, b) } else { (b, a) }
    }

    pub(super) fn set_caret(&self, c: &Caret, ins: &gtk::TextIter, bound: &gtk::TextIter) {
        if *c == self.primary() {
            self.buffer.select_range(ins, bound);
        } else {
            self.buffer.move_mark(&c.ins, ins);
            self.buffer.move_mark(&c.bound, bound);
        }
    }

    pub fn add_caret(&self, ins: &gtk::TextIter, bound: &gtk::TextIter) {
        let c = Caret {
            ins: self.buffer.create_mark(None, ins, false),
            bound: self.buffer.create_mark(None, bound, false),
        };
        self.pview().imp().extras.borrow_mut().push(c);
        self.refresh_carets();
    }

    fn drop_caret(&self, c: &Caret) {
        self.pview().imp().extras.borrow_mut().retain(|x| x != c);
        self.buffer.delete_mark(&c.ins);
        self.buffer.delete_mark(&c.bound);
    }

    pub fn clear_carets(&self) {
        let extras = std::mem::take(&mut *self.pview().imp().extras.borrow_mut());
        if extras.is_empty() {
            return;
        }
        for c in extras {
            self.buffer.delete_mark(&c.ins);
            self.buffer.delete_mark(&c.bound);
        }
        self.refresh_carets();
    }

    /// Remove the most recently added extra caret.
    pub fn pop_caret(&self) {
        let last = self.pview().imp().extras.borrow().last().cloned();
        if let Some(c) = last {
            self.drop_caret(&c);
            self.refresh_carets();
        }
    }

    /// Alt+Click: remove the extra caret under the pointer, or keep the current
    /// caret and move the primary one here.
    pub fn toggle_caret_at(&self, it: &gtk::TextIter) {
        let hit = self.extras().into_iter().find(|c| self.buffer.iter_at_mark(&c.ins).offset() == it.offset());
        match hit {
            Some(c) => {
                self.drop_caret(&c);
                self.refresh_carets();
            }
            None => {
                let (ins, bound) = (self.buffer.iter_at_mark(&self.buffer.get_insert()),
                                    self.buffer.iter_at_mark(&self.buffer.selection_bound()));
                if ins.offset() == it.offset() {
                    return;
                }
                let (io, bo) = (ins.offset(), bound.offset());
                self.buffer.place_cursor(it);
                self.add_caret(&self.buffer.iter_at_offset(io), &self.buffer.iter_at_offset(bo));
            }
        }
    }

    /// Merge carets that touch or overlap, re-tag extra selections and redraw.
    pub(super) fn refresh_carets(&self) {
        let primary = self.primary();
        let mut all: Vec<(Caret, i32, i32)> = self
            .carets()
            .into_iter()
            .map(|c| {
                let (s, e) = self.range_of(&c);
                (c, s.offset(), e.offset())
            })
            .collect();
        all.sort_by_key(|(_, s, e)| (*s, *e));
        let mut kept: Vec<(Caret, i32, i32)> = Vec::new();
        for cur in all {
            match kept.last_mut() {
                Some(prev) if cur.1 < prev.2 || cur.1 == prev.1 => {
                    // overlapping: keep the primary, else the earlier one, spanning both
                    let (s, e) = (prev.1.min(cur.1), prev.2.max(cur.2));
                    let (keep, gone) = if cur.0 == primary { (cur.0, prev.0.clone()) } else { (prev.0.clone(), cur.0) };
                    self.drop_caret(&gone);
                    if e > prev.2 || s < prev.1 {
                        let fwd = self.buffer.iter_at_mark(&keep.ins).offset() >= self.buffer.iter_at_mark(&keep.bound).offset();
                        let (si, ei) = (self.buffer.iter_at_offset(s), self.buffer.iter_at_offset(e));
                        if fwd { self.set_caret(&keep, &ei, &si) } else { self.set_caret(&keep, &si, &ei) }
                    }
                    *prev = (keep, s, e);
                }
                _ => kept.push(cur),
            }
        }
        let (s, e) = self.buffer.bounds();
        self.buffer.remove_tag(&self.tag_caret_sel, &s, &e);
        for c in self.extras() {
            let (s, e) = self.range_of(&c);
            if s != e {
                self.buffer.apply_tag(&self.tag_caret_sel, &s, &e);
            }
        }
        self.view.queue_draw();
    }

    /// Run `f` with replication off and as one undo step.
    pub(super) fn edit<R>(&self, f: impl FnOnce() -> R) -> R {
        let was = self.replicating.replace(true);
        self.buffer.begin_user_action();
        let r = f();
        self.buffer.end_user_action();
        self.replicating.set(was);
        self.refresh_carets();
        r
    }

    // -- motion ------------------------------------------------------------

    fn motion_target(&self, it: &gtk::TextIter, m: Motion) -> gtk::TextIter {
        let buf = self.buffer.upcast_ref::<gtk::TextBuffer>();
        let mut t = *it;
        match m {
            Motion::Left => {
                t.backward_char();
            }
            Motion::Right => {
                t.forward_char();
            }
            Motion::Up | Motion::Down | Motion::PageUp | Motion::PageDown => {
                let page = {
                    let h = self.view.iter_location(it).height().max(1);
                    (self.view.visible_rect().height() / h).max(1)
                };
                let delta = match m {
                    Motion::Up => -1,
                    Motion::Down => 1,
                    Motion::PageUp => -page,
                    _ => page,
                };
                let line = it.line() + delta;
                t = if line < 0 {
                    buf.start_iter()
                } else if line >= buf.line_count() {
                    buf.end_iter()
                } else {
                    iter_at_line_col(buf, line, it.line_offset())
                };
            }
            Motion::LineStart => {
                let fnb = first_non_blank(it);
                t = if fnb.offset() == it.offset() { let mut s = *it; s.set_line_offset(0); s } else { fnb };
            }
            Motion::LineEnd => t = line_end(it),
            Motion::WordLeft => {
                t.backward_word_start();
            }
            Motion::WordRight => {
                t.forward_word_end();
            }
            Motion::DocStart => t = buf.start_iter(),
            Motion::DocEnd => t = buf.end_iter(),
        }
        t
    }

    pub fn move_carets(&self, m: Motion, extend: bool) {
        for c in self.carets() {
            let ins = self.buffer.iter_at_mark(&c.ins);
            let (s, e) = self.range_of(&c);
            let target = match m {
                Motion::Left if !extend && s != e => s,
                Motion::Right if !extend && s != e => e,
                _ => self.motion_target(&ins, m),
            };
            let bound = if extend { self.buffer.iter_at_mark(&c.bound) } else { target };
            self.set_caret(&c, &target, &bound);
        }
        self.refresh_carets();
        self.view.scroll_mark_onscreen(&self.buffer.get_insert());
    }

    // -- editing at every caret -------------------------------------------

    pub fn delete_at_carets(&self, kind: Deletion) {
        self.edit(|| {
            for c in self.carets() {
                let (mut s, mut e) = self.range_of(&c);
                if s == e {
                    let it = s;
                    let other = match kind {
                        Deletion::CharLeft => self.motion_target(&it, Motion::Left),
                        Deletion::CharRight => self.motion_target(&it, Motion::Right),
                        Deletion::WordLeft => self.motion_target(&it, Motion::WordLeft),
                        Deletion::WordRight => self.motion_target(&it, Motion::WordRight),
                        Deletion::LineEnd if it.ends_line() => self.motion_target(&it, Motion::Right),
                        Deletion::LineEnd => line_end(&it),
                    };
                    (s, e) = if other.offset() < it.offset() { (other, it) } else { (it, other) };
                }
                self.buffer.delete(&mut s, &mut e);
            }
        });
    }

    /// Replace every caret's selection with `texts[i]` (in buffer order), or
    /// with `texts[0]` everywhere when the counts differ.
    pub fn insert_at_carets(&self, texts: &[String]) {
        self.edit(|| {
            let carets = self.carets_sorted();
            for (i, c) in carets.iter().enumerate() {
                let text = if texts.len() == carets.len() { &texts[i] } else { &texts[0] };
                let (mut s, mut e) = self.range_of(c);
                self.buffer.delete(&mut s, &mut e);
                let mut at = self.buffer.iter_at_mark(&c.ins);
                self.buffer.insert(&mut at, text);
                let at = self.buffer.iter_at_mark(&c.ins);
                self.set_caret(c, &at, &at);
            }
        });
    }

    // -- replication of edits made by GtkTextView at the primary caret ----

    pub(super) fn before_insert(&self, at: &gtk::TextIter) {
        self.mark_active.set(false);
        if self.replicating.get() || !self.has_extra_carets() {
            return;
        }
        let ins = self.buffer.iter_at_mark(&self.buffer.get_insert());
        self.pending_insert.set(ins.offset() == at.offset());
    }

    pub(super) fn after_insert(&self, text: &str) {
        if self.pending_insert.replace(false) {
            self.queue_replay(Replay::Insert(text.to_owned()));
        }
    }

    pub(super) fn before_delete(&self, s: &gtk::TextIter, e: &gtk::TextIter) {
        if self.replicating.get() || !self.has_extra_carets() {
            return;
        }
        let ins = self.buffer.iter_at_mark(&self.buffer.get_insert()).offset();
        let bound = self.buffer.iter_at_mark(&self.buffer.selection_bound()).offset();
        let (s, e) = (s.offset().min(e.offset()), s.offset().max(e.offset()));
        let pending = if ins != bound && (s, e) == (ins.min(bound), ins.max(bound)) {
            Some(PendingDelete::Selection)
        } else if s <= ins && ins <= e {
            Some(PendingDelete::Around(ins - s, e - ins))
        } else {
            None
        };
        self.pending_delete.set(pending);
    }

    pub(super) fn after_delete(&self) {
        if let Some(p) = self.pending_delete.take() {
            self.queue_replay(Replay::Delete(p));
        }
    }

    /// Edits are replayed when the user action that made them ends: code like
    /// completion holds iterators across several edits, and changing the
    /// buffer from inside its signal handlers would invalidate them.
    fn queue_replay(&self, r: Replay) {
        self.replay.borrow_mut().push(r);
        if !self.in_user_action.get() {
            self.flush_replay();
        }
    }

    pub(super) fn flush_replay(&self) {
        let ops = std::mem::take(&mut *self.replay.borrow_mut());
        if ops.is_empty() || self.replicating.get() {
            return;
        }
        self.edit(|| {
            for c in self.extras() {
                for op in &ops {
                    let (mut s, mut e) = self.range_of(&c);
                    match op {
                        Replay::Insert(text) => {
                            if s != e {
                                self.buffer.delete(&mut s, &mut e);
                            }
                            let mut at = self.buffer.iter_at_mark(&c.ins);
                            self.buffer.insert(&mut at, text);
                            let at = self.buffer.iter_at_mark(&c.ins);
                            self.buffer.move_mark(&c.bound, &at);
                        }
                        Replay::Delete(p) => {
                            if s == e {
                                let PendingDelete::Around(before, after) = *p else { continue };
                                let at = s.offset();
                                s = self.buffer.iter_at_offset((at - before).max(0));
                                e = self.buffer.iter_at_offset(at + after);
                            }
                            self.buffer.delete(&mut s, &mut e);
                        }
                    }
                }
            }
        });
    }

    /// Completion with several carets: replace the word around every caret
    /// with the (expanded) snippet text and select its first placeholder.
    pub(super) fn complete_at_carets(&self, text: &str, snippet: bool, before: i32, after: i32) {
        let (plain, stop) = if snippet { expand_snippet(text) } else { (text.to_owned(), None) };
        self.edit(|| {
            for c in self.carets() {
                let at = self.buffer.iter_at_mark(&c.ins).offset();
                let mut s = self.buffer.iter_at_offset((at - before).max(0));
                let mut e = self.buffer.iter_at_offset(at + after);
                let start = s.offset();
                self.buffer.delete(&mut s, &mut e);
                let mut it = self.buffer.iter_at_offset(start);
                self.buffer.insert(&mut it, &plain);
                let (a, b) = match stop {
                    Some((off, len)) => (start + off, start + off + len),
                    None => (start + plain.chars().count() as i32, start + plain.chars().count() as i32),
                };
                self.set_caret(&c, &self.buffer.iter_at_offset(b), &self.buffer.iter_at_offset(a));
            }
        });
    }
}

/// Expand an LSP snippet to plain text; also return the first tab stop's
/// (char offset, length) in it.
fn expand_snippet(text: &str) -> (String, Option<(i32, i32)>) {
    let re = regex::Regex::new(r"\$\{(\d+):?([^}]*)\}|\$(\d+)").unwrap();
    let mut out = String::new();
    let mut stops: Vec<(u32, i32, i32)> = Vec::new();
    let mut last = 0;
    for m in re.captures_iter(text) {
        let whole = m.get(0).unwrap();
        out.push_str(&text[last..whole.start()]);
        let n: u32 = m.get(1).or(m.get(3)).and_then(|g| g.as_str().parse().ok()).unwrap_or(0);
        let default = m.get(2).map_or("", |g| g.as_str());
        stops.push((if n == 0 { u32::MAX } else { n }, out.chars().count() as i32, default.chars().count() as i32));
        out.push_str(default);
        last = whole.end();
    }
    out.push_str(&text[last..]);
    let first = stops.into_iter().min_by_key(|s| s.0).map(|(_, off, len)| (off, len));
    (out, first)
}
