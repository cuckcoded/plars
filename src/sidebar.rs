//! Sidebar pages: project file tree, document outline, find/replace in project.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

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
    win: RefCell<Weak<Window>>,
}

fn file_of(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast::<gio::File>()
}

fn dir_model(dir: &gio::File) -> gio::ListModel {
    let list = gtk::DirectoryList::new(Some(ATTRS), Some(dir));
    let filter = gtk::CustomFilter::new(|o| {
        let info = o.downcast_ref::<gio::FileInfo>().unwrap();
        !(info.is_hidden() || info.name().to_string_lossy().starts_with('.'))
    });
    let sorter = gtk::CustomSorter::new(|a, b| {
        let a = a.downcast_ref::<gio::FileInfo>().unwrap();
        let b = b.downcast_ref::<gio::FileInfo>().unwrap();
        let (da, db) = (a.file_type() == gio::FileType::Directory, b.file_type() == gio::FileType::Directory);
        if da != db {
            return if da { gtk::Ordering::Smaller } else { gtk::Ordering::Larger };
        }
        a.display_name().to_lowercase().cmp(&b.display_name().to_lowercase()).into()
    });
    gtk::SortListModel::new(Some(gtk::FilterListModel::new(Some(list), Some(filter))), Some(sorter)).upcast()
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
        let tree = Rc::new(FileTree { root, list, selection, project: RefCell::default(), win: RefCell::default() });

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
        let model = gtk::TreeListModel::new(dir_model(&gio::File::for_path(path)), false, false, |o| {
            let info = o.downcast_ref::<gio::FileInfo>()?;
            if info.file_type() == gio::FileType::Directory { Some(dir_model(&file_of(info)?)) } else { None }
        });
        self.selection.set_model(Some(&model));
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
                if p.is_dir() {
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
