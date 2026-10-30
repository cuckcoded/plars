//! Bottom panel pages: Problems (tinymist diagnostics) and the server log.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk::prelude::*;
use serde_json::Value;

use crate::util::{relative, uri_to_path};

type JumpCb = Rc<dyn Fn(PathBuf, i64, i64)>;

pub struct ProblemsPanel {
    pub root: gtk::Box,
    list: gtk::ListBox,
    empty: gtk::Label,
    by_uri: RefCell<BTreeMap<String, Vec<Value>>>,
    locations: Rc<RefCell<Vec<(PathBuf, i64, i64)>>>,
    pub project_root: RefCell<Option<PathBuf>>,
}

impl ProblemsPanel {
    pub fn new(on_jump: impl Fn(PathBuf, i64, i64) + 'static) -> Rc<ProblemsPanel> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
        let empty = gtk::Label::builder()
            .label("No problems — the document compiles cleanly.")
            .css_classes(["dim-label"])
            .margin_top(18)
            .build();
        root.append(&empty);
        root.append(&gtk::ScrolledWindow::builder().vexpand(true).child(&list).build());
        let locations: Rc<RefCell<Vec<(PathBuf, i64, i64)>>> = Rc::default();
        let locs = locations.clone();
        let jump: JumpCb = Rc::new(on_jump);
        list.connect_row_activated(move |_, row| {
            let loc = locs.borrow().get(row.index() as usize).cloned();
            if let Some((p, l, c)) = loc {
                jump(p, l, c);
            }
        });
        Rc::new(ProblemsPanel { root, list, empty, by_uri: RefCell::default(), locations, project_root: RefCell::default() })
    }

    pub fn set(&self, uri: &str, diags: &[Value]) {
        if diags.is_empty() {
            self.by_uri.borrow_mut().remove(uri);
        } else {
            self.by_uri.borrow_mut().insert(uri.to_owned(), diags.to_vec());
        }
        self.rebuild();
    }

    pub fn clear(&self) {
        self.by_uri.borrow_mut().clear();
        self.rebuild();
    }

    /// (errors, warnings)
    pub fn counts(&self) -> (usize, usize) {
        let map = self.by_uri.borrow();
        let all = map.values().flatten();
        let errors = all.clone().filter(|d| d["severity"].as_i64().unwrap_or(1) == 1).count();
        (errors, all.count() - errors)
    }

    fn rebuild(&self) {
        self.list.remove_all();
        let mut locs = self.locations.borrow_mut();
        locs.clear();
        let root = self.project_root.borrow().clone();
        for (uri, diags) in self.by_uri.borrow().iter() {
            let path = uri_to_path(uri).unwrap_or_else(|| PathBuf::from(uri));
            let name = relative(&path, root.as_deref());
            let mut diags: Vec<&Value> = diags.iter().collect();
            diags.sort_by_key(|d| crate::util::pos_of(&d["range"]["start"]));
            for d in diags {
                let (line, col) = crate::util::pos_of(&d["range"]["start"]);
                let err = d["severity"].as_i64().unwrap_or(1) == 1;
                let msg = d["message"].as_str().unwrap_or("");
                let row = gtk::Box::builder().spacing(8).margin_start(6).margin_end(6).build();
                let icon = gtk::Image::from_icon_name(if err { "dialog-error-symbolic" } else { "dialog-warning-symbolic" });
                icon.add_css_class(if err { "error" } else { "warning" });
                row.append(&icon);
                row.append(&gtk::Label::builder()
                    .label(msg.lines().next().unwrap_or(""))
                    .tooltip_text(msg)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build());
                row.append(&gtk::Label::builder()
                    .label(format!("{name}:{}:{}", line + 1, col + 1))
                    .css_classes(["dim-label", "caption", "monospace"])
                    .build());
                self.list.append(&row);
                locs.push((path.clone(), line, col));
            }
        }
        self.empty.set_visible(locs.is_empty());
    }
}

pub struct LogPanel {
    pub root: gtk::Box,
    view: gtk::TextView,
    scroller: gtk::ScrolledWindow,
    filter: gtk::SearchEntry,
    lines: RefCell<Vec<String>>,
}

impl LogPanel {
    const MAX: usize = 5000;

    pub fn new() -> Rc<LogPanel> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let view = gtk::TextView::builder()
            .editable(false)
            .monospace(true)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .left_margin(6)
            .build();
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).child(&view).build();
        let bar = gtk::Box::builder().spacing(6).margin_start(6).margin_top(2).margin_bottom(2).build();
        let filter = gtk::SearchEntry::builder().placeholder_text("Filter…").hexpand(true).build();
        let clear = gtk::Button::builder().icon_name("edit-clear-all-symbolic").tooltip_text("Clear").css_classes(["flat"]).build();
        bar.append(&filter);
        bar.append(&clear);
        root.append(&bar);
        root.append(&scroller);
        let panel = Rc::new(LogPanel { root, view, scroller, filter, lines: RefCell::default() });
        let w = Rc::downgrade(&panel);
        panel.filter.connect_search_changed(move |_| {
            if let Some(p) = w.upgrade() {
                p.refilter();
            }
        });
        let w = Rc::downgrade(&panel);
        clear.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                p.lines.borrow_mut().clear();
                p.refilter();
            }
        });
        panel
    }

    pub fn append(&self, line: &str) {
        let overflow = {
            let mut lines = self.lines.borrow_mut();
            lines.push(line.to_owned());
            let over = lines.len() > Self::MAX;
            if over {
                let n = lines.len() - Self::MAX;
                lines.drain(..n);
            }
            over
        };
        if overflow {
            self.refilter();
            return;
        }
        let f = self.filter.text().to_lowercase();
        if !f.is_empty() && !line.to_lowercase().contains(&f) {
            return;
        }
        let buf = self.view.buffer();
        let adj = self.scroller.vadjustment();
        let at_bottom = adj.value() >= adj.upper() - adj.page_size() - 20.0;
        buf.insert(&mut buf.end_iter(), &format!("{line}\n"));
        if at_bottom {
            self.view.scroll_to_iter(&mut buf.end_iter(), 0.0, false, 0.0, 0.0);
        }
    }

    fn refilter(&self) {
        let f = self.filter.text().to_lowercase();
        let lines = self.lines.borrow();
        let mut s = lines
            .iter()
            .map(String::as_str)
            .filter(|l| f.is_empty() || l.to_lowercase().contains(&f))
            .collect::<Vec<_>>()
            .join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        self.view.buffer().set_text(&s);
    }
}
