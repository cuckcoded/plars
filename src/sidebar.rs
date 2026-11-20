//! Sidebar pages: project file tree, document outline, find/replace in project.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::Value;

use crate::window::Window;

const ATTRS: &str = "standard::name,standard::display-name,standard::type,standard::is-hidden,standard::symbolic-icon";
const SEARCH_EXTS: &[&str] = &["typ", "bib", "yml", "yaml", "toml", "txt", "md", "csv", "json", "xml", "svg"];

// ------------------------------------------------------------------ files

pub struct FileTree {
    pub root: gtk::Box,
    list: gtk::ListView,
    selection: gtk::SingleSelection,
    project: RefCell<Option<PathBuf>>,
    /// The listed directories: the root and every expanded folder.
    dirs: Rc<RefCell<Vec<Rc<DirModel>>>>,
    win: RefCell<Weak<Window>>,
}

fn file_of(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast::<gio::File>()
}

fn is_dir(info: &gio::FileInfo) -> bool {
    info.file_type() == gio::FileType::Directory
}

/// Folders first, then by name.
fn order(a: &gio::FileInfo, b: &gio::FileInfo) -> std::cmp::Ordering {
    is_dir(b).cmp(&is_dir(a))
        .then_with(|| a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()))
        .then_with(|| a.name().cmp(&b.name()))
}

/// The visible entries of one directory, kept current by a file monitor.
///
/// This replaces GtkDirectoryList, which replays monitor events in order
/// and stops for good at one it can't look up: a temporary file renamed away
/// before GTK got to it (every safe-save, including ours, does that) froze
/// the folder. Here an event only schedules a re-read of the directory, and
/// the result is merged into the list so rows that stay keep their identity,
/// and with it their expanded state and the selection.
struct DirModel {
    dir: gio::File,
    store: glib::WeakRef<gio::ListStore>,
    /// All listed folders, to revive children whose folder was replaced.
    dirs: Weak<RefCell<Vec<Rc<DirModel>>>>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    /// The folder went away, taking the monitor with it; watch again once
    /// it's back (deleted and recreated, e.g. by a git checkout).
    stale: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
}

impl DirModel {
    /// List `dir` now and watch it; the model lives as long as the store.
    fn open(dirs: &Rc<RefCell<Vec<Rc<DirModel>>>>, dir: gio::File) -> gio::ListStore {
        let store = gio::ListStore::new::<gio::FileInfo>();
        let model = Rc::new(DirModel {
            dir,
            store: store.downgrade(),
            dirs: Rc::downgrade(dirs),
            monitor: RefCell::default(),
            stale: Cell::new(false),
            timer: RefCell::default(),
        });
        model.watch();
        model.rescan();
        let mut dirs = dirs.borrow_mut();
        dirs.retain(|d| d.alive());
        dirs.push(model);
        store
    }

    fn watch(self: &Rc<Self>) {
        let monitor = self.dir.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE).ok();
        if let Some(m) = &monitor {
            let w = Rc::downgrade(self);
            m.connect_changed(move |_, file, _, event| {
                use gio::FileMonitorEvent as E;
                // content changes don't affect the listing
                if matches!(event, E::Changed | E::ChangesDoneHint) {
                    return;
                }
                let Some(m) = w.upgrade() else { return };
                if file.equal(&m.dir) && matches!(event, E::Deleted | E::MovedOut | E::Renamed | E::Unmounted) {
                    m.stale.set(true);
                }
                m.schedule();
            });
        }
        *self.monitor.borrow_mut() = monitor;
    }

    /// False once the tree has dropped the folder (collapsed or removed).
    fn alive(&self) -> bool {
        self.store.upgrade().is_some()
    }

    /// Re-read shortly; a burst of events (a save, a checkout) reads once.
    fn schedule(self: &Rc<Self>) {
        if self.timer.borrow().is_some() {
            return;
        }
        let w = Rc::downgrade(self);
        *self.timer.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_millis(100), move || {
            if let Some(m) = w.upgrade() {
                m.timer.borrow_mut().take();
                m.rescan();
            }
        }));
    }

    fn rescan(self: &Rc<Self>) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
        let Some(store) = self.store.upgrade() else { return };
        if self.stale.get() && self.dir.query_exists(gio::Cancellable::NONE) {
            // watch before listing so nothing in between is missed
            self.stale.set(false);
            self.watch();
        }
        let mut entries = Vec::new();
        match self.dir.enumerate_children(ATTRS, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
            Ok(en) => {
                while let Ok(Some(info)) = en.next_file(gio::Cancellable::NONE) {
                    if info.is_hidden() || info.name().to_string_lossy().starts_with('.') {
                        continue;
                    }
                    info.set_attribute_object("standard::file", &en.child(&info));
                    entries.push(info);
                }
            }
            Err(_) => self.stale.set(true),
        }
        entries.sort_by(order);
        Self::merge(&store, entries);

        // subfolders that were replaced: the rows stay, their contents and
        // monitors are renewed
        let Some(dirs) = self.dirs.upgrade() else { return };
        let revive: Vec<Rc<DirModel>> = dirs.borrow().iter()
            .filter(|d| d.stale.get() && d.alive() && d.dir.parent().is_some_and(|p| p.equal(&self.dir)))
            .cloned()
            .collect();
        for d in revive {
            d.rescan();
        }
    }

    /// Make `store` hold `entries`, keeping the rows that stay.
    fn merge(store: &gio::ListStore, entries: Vec<gio::FileInfo>) {
        // keep the existing object for an entry that is still the same kind
        let old: HashMap<PathBuf, gio::FileInfo> = (0..store.n_items())
            .filter_map(|i| store.item(i).and_downcast::<gio::FileInfo>())
            .map(|o| (o.name(), o))
            .collect();
        let target: Vec<gio::FileInfo> = entries.into_iter()
            .map(|new| match old.get(&new.name()) {
                Some(o) if is_dir(o) == is_dir(&new) => o.clone(),
                _ => new,
            })
            .collect();
        let kept: HashSet<usize> = target.iter().filter(|t| old.get(&t.name()) == Some(t)).map(|t| t.as_ptr() as usize).collect();
        if kept.is_empty() {
            store.splice(0, store.n_items(), &target);
            return;
        }
        // both lists are in the same order, so kept rows line up: only remove
        // what's gone and insert what's new, never touching the rest
        let (mut pos, mut j) = (0u32, 0usize);
        loop {
            let cur = store.item(pos).and_downcast::<gio::FileInfo>();
            match (cur, target.get(j)) {
                (None, None) => break,
                (Some(c), Some(t)) if c == *t => (pos, j) = (pos + 1, j + 1),
                (Some(c), _) if !kept.contains(&(c.as_ptr() as usize)) => store.remove(pos),
                (_, Some(t)) if !kept.contains(&(t.as_ptr() as usize)) => {
                    store.insert(pos, t);
                    (pos, j) = (pos + 1, j + 1);
                }
                _ => {
                    // the order of kept rows changed (a case-only rename)
                    store.splice(0, store.n_items(), &target);
                    break;
                }
            }
        }
    }
}

/// The path of the row at `pos`.
fn row_path(model: &gtk::TreeListModel, pos: u32) -> Option<PathBuf> {
    let row = model.item(pos).and_downcast::<gtk::TreeListRow>()?;
    row.item().and_downcast::<gio::FileInfo>().as_ref().and_then(file_of).and_then(|f| f.path())
}

impl FileTree {
    pub fn new() -> Rc<FileTree> {
        let selection = gtk::SingleSelection::builder().autoselect(false).can_unselect(true).build();
        let factory = gtk::SignalListItemFactory::new();
        let list = gtk::ListView::builder()
            .model(&selection)
            .factory(&factory)
            .css_classes(["navigation-sidebar"])
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&gtk::ScrolledWindow::builder().vexpand(true).child(&list).build());
        let tree = Rc::new(FileTree {
            root, list, selection, project: RefCell::default(), dirs: Rc::default(), win: RefCell::default(),
        });

        let w = Rc::downgrade(&tree);
        factory.connect_setup(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&gtk::Image::new());
            row.append(&gtk::Label::builder().xalign(0.0).ellipsize(gtk::pango::EllipsizeMode::Middle).build());
            let expander = gtk::TreeExpander::builder().child(&row).build();
            let click = gtk::GestureClick::builder().button(3).build();
            let (w, li) = (w.clone(), item.downgrade());
            click.connect_pressed(move |g, _, x, y| {
                g.set_state(gtk::EventSequenceState::Claimed);
                let (Some(t), Some(li)) = (w.upgrade(), li.upgrade()) else { return };
                let Some(row) = li.item().and_downcast::<gtk::TreeListRow>() else { return };
                let Some(path) = row.item().and_downcast::<gio::FileInfo>().as_ref().and_then(file_of).and_then(|f| f.path()) else { return };
                t.popup(g.widget().as_ref().unwrap(), x, y, &path);
            });
            expander.add_controller(click);
            item.set_child(Some(&expander));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else { return };
            let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
            expander.set_list_row(Some(&row));
            let Some(info) = row.item().and_downcast::<gio::FileInfo>() else { return };
            let bx = expander.child().unwrap();
            let icon = bx.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = bx.last_child().and_downcast::<gtk::Label>().unwrap();
            let name = info.display_name();
            label.set_label(&name);
            if info.file_type() == gio::FileType::Directory {
                icon.set_icon_name(Some("folder-symbolic"));
            } else if name.ends_with(".typ") {
                icon.set_icon_name(Some("text-x-generic-symbolic"));
            } else if let Some(g) = info.symbolic_icon() {
                icon.set_from_gicon(&g);
            }
        });

        let w = Rc::downgrade(&tree);
        tree.list.connect_activate(move |lv, pos| {
            let Some(t) = w.upgrade() else { return };
            let Some(row) = lv.model().and_then(|m| m.item(pos)).and_downcast::<gtk::TreeListRow>() else { return };
            let Some(info) = row.item().and_downcast::<gio::FileInfo>() else { return };
            if info.file_type() == gio::FileType::Directory {
                row.set_expanded(!row.is_expanded());
            } else if let (Some(path), Some(win)) = (file_of(&info).and_then(|f| f.path()), t.win.borrow().upgrade()) {
                win.open_file(&path);
            }
        });

        // right-click on empty space acts on the project root
        let click = gtk::GestureClick::builder().button(3).build();
        let w = Rc::downgrade(&tree);
        click.connect_pressed(move |g, _, x, y| {
            let Some(t) = w.upgrade() else { return };
            let root = t.project.borrow().clone();
            if let Some(root) = root {
                t.popup(&t.list.clone().upcast(), x, y, &root);
            }
            let _ = g;
        });
        tree.list.add_controller(click);
        tree
    }

    pub fn set_window(&self, w: Weak<Window>) {
        *self.win.borrow_mut() = w;
    }

    pub fn set_root(&self, path: &Path) {
        *self.project.borrow_mut() = Some(path.to_path_buf());
        self.dirs.borrow_mut().clear();
        let dirs = self.dirs.clone();
        let root = DirModel::open(&self.dirs, gio::File::for_path(path));
        let model = gtk::TreeListModel::new(root, false, false, move |o| {
            let info = o.downcast_ref::<gio::FileInfo>()?;
            if is_dir(info) { Some(DirModel::open(&dirs, file_of(info)?).upcast()) } else { None }
        });
        self.selection.set_model(Some(&model));
    }

    /// Re-read every listed folder, e.g. when the window regains focus, in
    /// case a change slipped past the file monitors (network drives, a
    /// full inotify queue).
    pub fn refresh(&self) {
        let dirs: Vec<Rc<DirModel>> = self.dirs.borrow().iter().filter(|d| d.alive()).cloned().collect();
        for d in dirs {
            d.rescan();
        }
    }

    /// Select `path` (expanding the folders above it) and scroll to it, or
    /// clear the selection when it isn't in the tree.
    pub fn reveal(&self, path: Option<&Path>) {
        match path.and_then(|p| self.expand_to(p)) {
            Some(pos) => {
                self.selection.set_selected(pos);
                self.list.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
            }
            None => self.selection.set_selected(gtk::INVALID_LIST_POSITION),
        }
    }

    fn expand_to(&self, path: &Path) -> Option<u32> {
        let root = self.project.borrow().clone()?;
        let rel = path.strip_prefix(&root).ok()?;
        let model = self.selection.model().and_downcast::<gtk::TreeListModel>()?;
        let find = |from: u32, target: &Path| (from..model.n_items()).find(|&i| row_path(&model, i).as_deref() == Some(target));
        let (mut cur, mut from, mut found) = (root, 0, None);
        for comp in rel.components() {
            let parent = cur.clone();
            cur.push(comp);
            // just created? the monitor may not have caught up yet
            let pos = find(from, &cur).or_else(|| {
                let dir = self.dirs.borrow().iter().find(|d| d.alive() && d.dir.path().as_ref() == Some(&parent)).cloned();
                dir?.rescan();
                find(from, &cur)
            })?;
            if cur != path {
                model.item(pos).and_downcast::<gtk::TreeListRow>()?.set_expanded(true);
            }
            (from, found) = (pos + 1, Some(pos));
        }
        found
    }

    fn popup(&self, widget: &gtk::Widget, x: f64, y: f64, path: &Path) {
        let is_dir = path.is_dir();
        let target = if is_dir { path.to_path_buf() } else { path.parent().unwrap().to_path_buf() };
        let menu = gio::Menu::new();
        let item = |label: &str, action: &str, p: &Path| {
            let i = gio::MenuItem::new(Some(label), None);
            i.set_action_and_target_value(Some(&format!("tree.{action}")), Some(&p.to_string_lossy().to_variant()));
            i
        };
        let s1 = gio::Menu::new();
        s1.append_item(&item("New File…", "new-file", &target));
        s1.append_item(&item("New Folder…", "new-folder", &target));
        menu.append_section(None, &s1);
        if Some(path) != self.project.borrow().as_deref() {
            let s2 = gio::Menu::new();
            if crate::util::is_typst(path) {
                s2.append_item(&item("Set as Main File (Preview & Debug)", "set-main", path));
            }
            s2.append_item(&item("Rename…", "rename", path));
            s2.append_item(&item("Move to Trash", "trash", path));
            menu.append_section(None, &s2);
        }
        let s3 = gio::Menu::new();
        s3.append_item(&item("Show in File Manager", "reveal", path));
        menu.append_section(None, &s3);

        let group = gio::SimpleActionGroup::new();
        for name in ["new-file", "new-folder", "set-main", "rename", "trash", "reveal"] {
            let act = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let w = self.win.borrow().clone();
            act.connect_activate(move |_, v| {
                let (Some(win), Some(p)) = (w.upgrade(), v.and_then(|v| v.get::<String>())) else { return };
                win.tree_action(name, &PathBuf::from(p));
            });
            group.add_action(&act);
        }
        self.root.insert_action_group("tree", Some(&group));

        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(widget);
        pop.set_has_arrow(false);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }
}

// ---------------------------------------------------------------- outline

pub struct Outline {
    pub root: gtk::Box,
    list: gtk::ListBox,
    empty: gtk::Label,
    positions: Rc<RefCell<Vec<(i64, i64)>>>,
}

impl Outline {
    pub fn new(on_jump: impl Fn(i64, i64) + 'static) -> Outline {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
        let empty = gtk::Label::builder().label("No headings in this file.").css_classes(["dim-label"]).margin_top(24).build();
        root.append(&empty);
        root.append(&gtk::ScrolledWindow::builder().vexpand(true).child(&list).build());
        let positions: Rc<RefCell<Vec<(i64, i64)>>> = Rc::default();
        let pos = positions.clone();
        list.connect_row_activated(move |_, row| {
            let p = pos.borrow().get(row.index() as usize).copied();
            if let Some((l, c)) = p {
                on_jump(l, c);
            }
        });
        Outline { root, list, empty, positions }
    }

    pub fn set_symbols(&self, symbols: &[Value]) {
        self.list.remove_all();
        self.positions.borrow_mut().clear();
        self.add(symbols, 0);
        self.empty.set_visible(self.positions.borrow().is_empty());
    }

    fn add(&self, symbols: &[Value], depth: i32) {
        for s in symbols {
            let range = [&s["selectionRange"], &s["range"], &s["location"]["range"]]
                .into_iter()
                .find(|r| !r.is_null())
                .cloned()
                .unwrap_or(Value::Null);
            let label = gtk::Label::builder()
                .label(s["name"].as_str().unwrap_or(""))
                .xalign(0.0)
                .margin_start(8 + depth * 14)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build();
            if depth == 0 {
                label.add_css_class("heading");
            }
            self.list.append(&label);
            self.positions.borrow_mut().push(crate::util::pos_of(&range["start"]));
            if let Some(children) = s["children"].as_array() {
                self.add(children, depth + 1);
            }
        }
    }
}

// --------------------------------------------------------- project search

pub struct ProjectSearch {
    pub root: gtk::Box,
    pub entry: gtk::SearchEntry,
    replace: gtk::Entry,
    case: gtk::ToggleButton,
    word: gtk::ToggleButton,
    regex: gtk::ToggleButton,
    summary: gtk::Label,
    results: gtk::ListBox,
    hits: RefCell<Vec<Option<(PathBuf, i32, i32, i32)>>>,
    win: RefCell<Weak<Window>>,
}

impl ProjectSearch {
    pub fn new() -> Rc<ProjectSearch> {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6)
            .margin_start(6).margin_end(6).margin_top(6).build();
        let s = Rc::new(ProjectSearch {
            root,
            entry: gtk::SearchEntry::builder().placeholder_text("Search the project").build(),
            replace: gtk::Entry::builder().placeholder_text("Replace with").build(),
            case: gtk::ToggleButton::builder().label("Aa").tooltip_text("Match case").build(),
            word: gtk::ToggleButton::builder().label("W").tooltip_text("Whole word").build(),
            regex: gtk::ToggleButton::builder().label(".*").tooltip_text("Regular expression").build(),
            summary: gtk::Label::builder().xalign(0.0).css_classes(["dim-label", "caption"]).wrap(true).build(),
            results: gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build(),
            hits: RefCell::default(),
            win: RefCell::default(),
        });
        let go = gtk::Button::builder().label("Find").hexpand(true).css_classes(["suggested-action"]).build();
        let replace_all = gtk::Button::builder().label("Replace All").tooltip_text("Replace all matches in the project").build();
        let opts = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        for w in [s.case.upcast_ref::<gtk::Widget>(), s.word.upcast_ref(), s.regex.upcast_ref(), go.upcast_ref(), replace_all.upcast_ref()] {
            opts.append(w);
        }
        s.root.append(&s.entry);
        s.root.append(&s.replace);
        s.root.append(&opts);
        s.root.append(&s.summary);
        s.root.append(&gtk::ScrolledWindow::builder().vexpand(true).child(&s.results).build());

        let w = Rc::downgrade(&s);
        s.entry.connect_activate(move |_| {
            if let Some(s) = w.upgrade() {
                s.run();
            }
        });
        let w = Rc::downgrade(&s);
        go.connect_clicked(move |_| {
            if let Some(s) = w.upgrade() {
                s.run();
            }
        });
        let w = Rc::downgrade(&s);
        replace_all.connect_clicked(move |_| {
            if let Some(s) = w.upgrade() {
                s.confirm_replace();
            }
        });
        let w = Rc::downgrade(&s);
        s.results.connect_row_activated(move |_, row| {
            let Some(s) = w.upgrade() else { return };
            let hit = s.hits.borrow().get(row.index() as usize).cloned().flatten();
            let win = s.win.borrow().upgrade();
            if let (Some((p, l, a, b)), Some(win)) = (hit, win) {
                win.open_and_select(&p, l, a, b);
            }
        });
        s
    }

    pub fn set_window(&self, w: Weak<Window>) {
        *self.win.borrow_mut() = w;
    }

    fn pattern(&self) -> Option<regex::Regex> {
        let text = self.entry.text();
        if text.is_empty() {
            return None;
        }
        let mut pat = if self.regex.is_active() { text.to_string() } else { regex::escape(&text) };
        if self.word.is_active() {
            pat = format!(r"\b{pat}\b");
        }
        match regex::RegexBuilder::new(&pat).case_insensitive(!self.case.is_active()).build() {
            Ok(r) => Some(r),
            Err(e) => {
                self.summary.set_label(&format!("Invalid pattern: {e}"));
                None
            }
        }
    }

    fn files(&self) -> Vec<PathBuf> {
        let Some(root) = self.win.borrow().upgrade().and_then(|w| w.project_root()) else { return vec![] };
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| SEARCH_EXTS.contains(&x.to_string_lossy().to_lowercase().as_str())) {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    fn read(&self, path: &Path) -> Option<String> {
        if let Some(page) = self.win.borrow().upgrade().and_then(|w| w.find_page(path)) {
            return Some(page.text());
        }
        std::fs::read_to_string(path).ok()
    }

    pub fn run(&self) {
        self.results.remove_all();
        self.hits.borrow_mut().clear();
        let Some(re) = self.pattern() else { return };
        let root = self.win.borrow().upgrade().and_then(|w| w.project_root());
        let (mut total, mut files) = (0usize, 0usize);
        'outer: for path in self.files() {
            let Some(text) = self.read(&path) else { continue };
            let mut header = false;
            for (lineno, line) in text.lines().enumerate() {
                for m in re.find_iter(line) {
                    if total >= 2000 {
                        break 'outer;
                    }
                    if !header {
                        header = true;
                        files += 1;
                        let l = gtk::Label::builder()
                            .label(crate::util::relative(&path, root.as_deref()))
                            .xalign(0.0)
                            .css_classes(["heading"])
                            .margin_top(6)
                            .build();
                        let row = gtk::ListBoxRow::builder().child(&l).activatable(false).selectable(false).build();
                        self.results.append(&row);
                        self.hits.borrow_mut().push(None);
                    }
                    total += 1;
                    let trimmed = line.trim_start();
                    let off = line.len() - trimmed.len();
                    let (a, b) = (m.start().saturating_sub(off), m.end().saturating_sub(off));
                    let before: String = trimmed[..a].chars().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect();
                    let after: String = trimmed[b..].chars().take(80).collect();
                    let label = gtk::Label::builder().xalign(0.0).margin_start(8).ellipsize(gtk::pango::EllipsizeMode::End).build();
                    label.set_markup(&format!(
                        "<span alpha='60%'>{}</span>  {}<b>{}</b>{}",
                        lineno + 1,
                        glib::markup_escape_text(&before),
                        glib::markup_escape_text(&trimmed[a..b]),
                        glib::markup_escape_text(&after)
                    ));
                    self.results.append(&label);
                    let cs = line[..m.start()].chars().count() as i32;
                    let ce = line[..m.end()].chars().count() as i32;
                    self.hits.borrow_mut().push(Some((path.clone(), lineno as i32, cs, ce)));
                }
            }
        }
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        self.summary.set_label(&format!(
            "{total} result{} in {files} file{}{}",
            plural(total),
            plural(files),
            if total >= 2000 { " (truncated)" } else { "" }
        ));
    }

    fn confirm_replace(self: &Rc<Self>) {
        let Some(re) = self.pattern() else { return };
        let Some(win) = self.win.borrow().upgrade() else { return };
        let dlg = adw::AlertDialog::new(
            Some("Replace in Project?"),
            Some(&format!(
                "Replace every match of “{}” with “{}” in all project files. Open files are changed in the \
                 editor (undoable); closed files are rewritten on disk.",
                self.entry.text(),
                self.replace.text()
            )),
        );
        dlg.add_responses(&[("cancel", "Cancel"), ("replace", "Replace All")]);
        dlg.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        let w = Rc::downgrade(self);
        dlg.connect_response(None, move |_, r| {
            if let (Some(s), "replace") = (w.upgrade(), r) {
                s.replace_all(&re);
            }
        });
        dlg.present(Some(&win.widget));
    }

    fn replace_all(&self, re: &regex::Regex) {
        let Some(win) = self.win.borrow().upgrade() else { return };
        let repl = self.replace.text().to_string();
        let mut changed = 0;
        for path in self.files() {
            let Some(text) = self.read(&path) else { continue };
            let n = re.find_iter(&text).count();
            if n == 0 {
                continue;
            }
            let new = if self.regex.is_active() {
                re.replace_all(&text, repl.as_str()).into_owned()
            } else {
                re.replace_all(&text, regex::NoExpand(&repl)).into_owned()
            };
            changed += n;
            if let Some(page) = win.find_page(&path) {
                page.buffer.begin_user_action();
                page.buffer.set_text(&new);
                page.buffer.end_user_action();
            } else if let Err(e) = std::fs::write(&path, new) {
                win.toast(&format!("{}: {e}", path.display()));
            }
        }
        win.toast(&format!("Replaced {changed} occurrence{}", if changed == 1 { "" } else { "s" }));
        self.run();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, dir: bool) -> gio::FileInfo {
        let i = gio::FileInfo::new();
        i.set_name(name);
        i.set_display_name(name);
        i.set_file_type(if dir { gio::FileType::Directory } else { gio::FileType::Regular });
        i
    }

    fn names(store: &gio::ListStore) -> Vec<String> {
        (0..store.n_items())
            .map(|i| store.item(i).and_downcast::<gio::FileInfo>().unwrap().name().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn merge_keeps_rows_that_stay() {
        let store = gio::ListStore::new::<gio::FileInfo>();
        let (a, b, c) = (info("a", true), info("b", false), info("c", false));
        store.extend_from_slice(&[a.clone(), b, c.clone()]);

        // b renamed to bb: a and c keep their objects (and so their expanded rows)
        DirModel::merge(&store, vec![info("a", true), info("bb", false), info("c", false)]);
        assert_eq!(names(&store), ["a", "bb", "c"]);
        assert_eq!(store.item(0).and_downcast::<gio::FileInfo>().unwrap(), a);
        assert_eq!(store.item(2).and_downcast::<gio::FileInfo>().unwrap(), c);

        // a folder replaced by a file of the same name gets a new row
        DirModel::merge(&store, vec![info("a", false), info("bb", false), info("c", false)]);
        assert_ne!(store.item(0).and_downcast::<gio::FileInfo>().unwrap(), a);
        assert_eq!(store.item(2).and_downcast::<gio::FileInfo>().unwrap(), c);

        DirModel::merge(&store, vec![]);
        assert_eq!(store.n_items(), 0);
    }
}
