//! One editor tab: a GtkSourceView bound to a file and wired to tinymist
//! (completion, hover, diagnostics), plus breakpoint marks and in-file search.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib, pango};
use serde_json::{json, Value};
use sourceview5::prelude::*;
use gtk::subclass::prelude::*;
use sourceview5::subclass::prelude::*;

mod carets;
mod commands;

pub use carets::PlarsView;

use crate::lsp::Lsp;
use crate::settings::Settings;
use crate::util::{apply_text_edits, is_typst, iter_at_lsp, lsp_position, markdown_to_pango, range_iters};

pub enum PageEvent {
    Modified,
    Cursor,
    TextChanged,
    Breakpoint,
    CtrlClick,
}

pub fn language_manager() -> sourceview5::LanguageManager {
    thread_local!(static LM: sourceview5::LanguageManager = {
        let lm = sourceview5::LanguageManager::default();
        let mut paths: Vec<String> = Vec::new();
        for dir in crate::data_dirs() {
            paths.push(dir.join("language-specs").to_string_lossy().into_owned());
        }
        paths.extend(lm.search_path().iter().map(|s| s.to_string()));
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        lm.set_search_path(&refs);
        lm
    });
    LM.with(|lm| lm.clone())
}

// ---------------------------------------------------------------- completion

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Proposal {
        pub item: RefCell<Value>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Proposal {
        const NAME: &'static str = "PlarsProposal";
        type Type = super::Proposal;
        type Interfaces = (sourceview5::CompletionProposal,);
    }
    impl ObjectImpl for Proposal {}
    impl CompletionProposalImpl for Proposal {}

    #[derive(Default)]
    pub struct CompletionProvider {
        pub page: RefCell<Weak<EditorPage>>,
        pub all: RefCell<Vec<super::Proposal>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CompletionProvider {
        const NAME: &'static str = "PlarsCompletionProvider";
        type Type = super::CompletionProvider;
        type Interfaces = (sourceview5::CompletionProvider,);
    }
    impl ObjectImpl for CompletionProvider {}

    impl CompletionProviderImpl for CompletionProvider {
        fn title(&self) -> Option<glib::GString> {
            Some("Typst".into())
        }

        fn priority(&self, _: &sourceview5::CompletionContext) -> i32 {
            100
        }

        fn is_trigger(&self, _: &gtk::TextIter, c: char) -> bool {
            "#.@<(:/".contains(c)
        }

        fn populate_future(
            &self,
            context: &sourceview5::CompletionContext,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<gio::ListModel, glib::Error>>>> {
            let obj = self.obj().clone();
            let page = self.page.borrow().upgrade();
            let pos = context.bounds().map(|(_, it)| it);
            let word = context.word().to_string();
            Box::pin(async move {
                let empty = || Ok(gio::ListStore::new::<super::Proposal>().upcast());
                let Some(page) = page else { return empty() };
                let Some(lsp) = page.lsp() else { return empty() };
                let (Some(it), true) = (pos, page.is_typst()) else { return empty() };
                page.flush_changes();
                let params = json!({ "textDocument": { "uri": page.uri() },
                                     "position": lsp_position(&page.buffer, &it) });
                let (tx, rx) = async_channel::bounded(1);
                lsp.request("textDocument/completion", params, move |r| {
                    let _ = tx.send_blocking(r);
                });
                let result = rx.recv().await.ok().and_then(Result::ok).unwrap_or(Value::Null);
                let mut items = match result {
                    Value::Object(mut o) => o.remove("items").and_then(|v| v.as_array().cloned()).unwrap_or_default(),
                    Value::Array(a) => a,
                    _ => vec![],
                };
                items.sort_by_key(|a| sort_key(a));
                let proposals: Vec<super::Proposal> = items.into_iter().map(super::Proposal::new).collect();
                *obj.imp().all.borrow_mut() = proposals;
                Ok(obj.filtered(&word).upcast())
            })
        }

        fn refilter(&self, context: &sourceview5::CompletionContext, _: &gio::ListModel) {
            let store = self.obj().filtered(&context.word());
            context.set_proposals_for_provider(&*self.obj(), Some(&store));
        }

        fn display(
            &self,
            _: &sourceview5::CompletionContext,
            proposal: &sourceview5::CompletionProposal,
            cell: &sourceview5::CompletionCell,
        ) {
            let Some(p) = proposal.downcast_ref::<super::Proposal>() else { return };
            let item = p.imp().item.borrow();
            match cell.column() {
                sourceview5::CompletionColumn::TypedText => cell.set_text(item["label"].as_str()),
                sourceview5::CompletionColumn::Icon => cell.set_icon_name(kind_icon(item["kind"].as_i64())),
                sourceview5::CompletionColumn::After => {
                    let d = item["labelDetails"]["description"].as_str().or(item["detail"].as_str()).unwrap_or("");
                    let d: String = if d.chars().count() > 60 { d.chars().take(57).chain("…".chars()).collect() } else { d.into() };
                    cell.set_text(Some(&d));
                }
                sourceview5::CompletionColumn::Details => {
                    let doc = match &item["documentation"] {
                        Value::String(s) => s.clone(),
                        Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or("").into(),
                        _ => item["detail"].as_str().unwrap_or("").into(),
                    };
                    cell.set_markup(&markdown_to_pango(&doc));
                }
                _ => cell.set_text(None),
            }
        }

        fn activate(&self, context: &sourceview5::CompletionContext, proposal: &sourceview5::CompletionProposal) {
            let Some(page) = self.page.borrow().upgrade() else { return };
            let Some(p) = proposal.downcast_ref::<super::Proposal>() else { return };
            let item = p.imp().item.borrow().clone();
            page.insert_completion(context, &item);
        }
    }

    // -------------------------------------------------------------- hover

    #[derive(Default)]
    pub struct HoverProvider {
        pub page: RefCell<Weak<EditorPage>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HoverProvider {
        const NAME: &'static str = "PlarsHoverProvider";
        type Type = super::HoverProvider;
        type Interfaces = (sourceview5::HoverProvider,);
    }
    impl ObjectImpl for HoverProvider {}

    impl HoverProviderImpl for HoverProvider {
        fn populate_future(
            &self,
            context: &sourceview5::HoverContext,
            display: &sourceview5::HoverDisplay,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), glib::Error>> + 'static>> {
            let page = self.page.borrow().upgrade();
            let it = context.bounds().map(|(s, _)| s);
            let display = display.clone();
            Box::pin(async move {
                let nothing = || Err(glib::Error::new(gio::IOErrorEnum::NotFound, "nothing to show"));
                let (Some(page), Some(it)) = (page, it) else { return nothing() };
                let mut shown = false;
                for d in page.diagnostics_at(&it) {
                    let err = d["severity"].as_i64().unwrap_or(1) == 1;
                    let label = gtk::Label::builder()
                        .label(format!("{}: {}", if err { "Error" } else { "Warning" }, d["message"].as_str().unwrap_or("")))
                        .wrap(true).xalign(0.0).max_width_chars(80).selectable(true).build();
                    label.add_css_class(if err { "error" } else { "warning" });
                    display.append(&label);
                    shown = true;
                }
                if let (Some(lsp), true) = (page.lsp(), page.is_typst()) {
                    let params = json!({ "textDocument": { "uri": page.uri() },
                                         "position": lsp_position(&page.buffer, &it) });
                    let (tx, rx) = async_channel::bounded(1);
                    lsp.request("textDocument/hover", params, move |r| {
                        let _ = tx.send_blocking(r);
                    });
                    let result = rx.recv().await.ok().and_then(Result::ok).unwrap_or(Value::Null);
                    let text = match &result["contents"] {
                        Value::String(s) => s.clone(),
                        Value::Object(o) => o.get("value").and_then(Value::as_str).unwrap_or("").into(),
                        Value::Array(a) => a
                            .iter()
                            .map(|c| c.as_str().or(c["value"].as_str()).unwrap_or("").to_owned())
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                        _ => String::new(),
                    };
                    if !text.trim().is_empty() {
                        let label = gtk::Label::builder().wrap(true).xalign(0.0).max_width_chars(80).selectable(true).build();
                        label.set_markup(&markdown_to_pango(&text));
                        display.append(&label);
                        shown = true;
                    }
                }
                if shown { Ok(()) } else { nothing() }
            })
        }
    }
}

glib::wrapper! {
    pub struct Proposal(ObjectSubclass<imp::Proposal>) @implements sourceview5::CompletionProposal;
}

impl Proposal {
    fn new(item: Value) -> Self {
        let p: Self = glib::Object::new();
        *p.imp().item.borrow_mut() = item;
        p
    }

    fn filter_text(&self) -> String {
        let item = self.imp().item.borrow();
        item["filterText"].as_str().or(item["label"].as_str()).unwrap_or("").to_lowercase()
    }
}

glib::wrapper! {
    pub struct CompletionProvider(ObjectSubclass<imp::CompletionProvider>) @implements sourceview5::CompletionProvider;
}

impl CompletionProvider {
    /// Substring matches of the typed word, prefix matches first.
    fn filtered(&self, word: &str) -> gio::ListStore {
        let store = gio::ListStore::new::<Proposal>();
        let word = word.to_lowercase();
        let all = self.imp().all.borrow();
        let (prefix, inner): (Vec<&Proposal>, Vec<&Proposal>) = all
            .iter()
            .filter(|p| word.is_empty() || p.filter_text().contains(&word))
            .partition(|p| p.filter_text().starts_with(&word));
        for p in prefix.into_iter().chain(inner) {
            store.append(p);
        }
        store
    }
}

glib::wrapper! {
    pub struct HoverProvider(ObjectSubclass<imp::HoverProvider>) @implements sourceview5::HoverProvider;
}

fn sort_key(item: &Value) -> String {
    item["sortText"].as_str().or(item["label"].as_str()).unwrap_or("").to_owned()
}

fn kind_icon(kind: Option<i64>) -> &'static str {
    match kind {
        Some(2) => "lang-method-symbolic",
        Some(3) => "lang-function-symbolic",
        Some(7) => "lang-struct-symbolic",
        Some(9) => "lang-namespace-symbolic",
        Some(10) | Some(5) => "lang-struct-field-symbolic",
        Some(13) | Some(20) => "lang-enum-value-symbolic",
        Some(14) | Some(15) => "lang-define-symbolic",
        Some(17) | Some(19) => "lang-include-symbolic",
        Some(21) => "lang-enum-symbolic",
        _ => "lang-variable-symbolic",
    }
}

// --------------------------------------------------------------- the page

pub struct EditorPage {
    pub root: gtk::Box,
    pub buffer: sourceview5::Buffer,
    pub view: sourceview5::View,
    pub vim_label: gtk::Label,
    path: RefCell<PathBuf>,
    settings: Rc<Settings>,
    lsp: RefCell<Option<Rc<Lsp>>>,
    version: Cell<i64>,
    diagnostics: RefCell<Vec<Value>>,
    change_timer: RefCell<Option<glib::SourceId>>,
    autosave_timer: RefCell<Option<glib::SourceId>>,
    saved_mtime: Cell<Option<std::time::SystemTime>>,
    on_event: RefCell<Option<Rc<dyn Fn(&Rc<EditorPage>, PageEvent)>>>,
    vim: RefCell<Option<gtk::EventControllerKey>>,
    // keymaps & multiple carets
    key_seq: RefCell<Vec<crate::keymap::Chord>>,
    mark_active: Cell<bool>,
    replicating: Cell<bool>,
    pending_insert: Cell<bool>,
    pending_delete: Cell<Option<carets::PendingDelete>>,
    replay: RefCell<Vec<carets::Replay>>,
    in_user_action: Cell<bool>,
    expand_stack: RefCell<Vec<((i32, i32), (i32, i32))>>,
    tag_caret_sel: gtk::TextTag,
    tag_error: gtk::TextTag,
    tag_warning: gtk::TextTag,
    tag_flash: gtk::TextTag,
    // in-file search
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    replace_row: gtk::Box,
    replace_entry: gtk::Entry,
    search_settings: sourceview5::SearchSettings,
    search_ctx: sourceview5::SearchContext,
}

impl EditorPage {
    pub fn new(path: &Path, settings: Rc<Settings>) -> Rc<EditorPage> {
        let lang = language_manager().guess_language(Some(path), None);
        let buffer = sourceview5::Buffer::new(None);
        buffer.set_language(lang.as_ref());
        buffer.set_highlight_matching_brackets(true);
        let view: sourceview5::View = glib::Object::builder::<PlarsView>()
            .property("buffer", &buffer)
            .property("monospace", true)
            .property("auto-indent", true)
            .property("indent-on-tab", true)
            .property("smart-backspace", true)
            .property("show-line-marks", true)
            .property("top-margin", 8)
            .property("bottom-margin", 200)
            .property("left-margin", 6)
            .build()
            .upcast();
        view.add_css_class("plars-editor");

        let tag_error = buffer.create_tag(Some("diag-error"), &[("underline", &pango::Underline::Error)]).unwrap();
        let tag_warning = buffer
            .create_tag(Some("diag-warning"), &[("underline", &pango::Underline::Error),
                ("underline-rgba", &gdk::RGBA::new(0.9, 0.6, 0.0, 1.0))])
            .unwrap();
        let tag_caret_sel = buffer
            .create_tag(Some("caret-selection"), &[("background-rgba", &gdk::RGBA::new(0.21, 0.52, 0.89, 0.30))])
            .unwrap();
        let tag_flash = buffer
            .create_tag(Some("flash"), &[("background-rgba", &gdk::RGBA::new(1.0, 0.85, 0.2, 0.35))])
            .unwrap();

        let bp = sourceview5::MarkAttributes::new();
        bp.set_icon_name("media-record-symbolic");
        bp.set_background(&gdk::RGBA::new(0.9, 0.2, 0.2, 0.12));
        view.set_mark_attributes("breakpoint", &bp, 10);
        let ex = sourceview5::MarkAttributes::new();
        ex.set_icon_name("go-next-symbolic");
        ex.set_background(&gdk::RGBA::new(1.0, 0.8, 0.1, 0.30));
        view.set_mark_attributes("exec", &ex, 20);

        // search bar
        let search_settings = sourceview5::SearchSettings::builder().wrap_around(true).build();
        let search_ctx = sourceview5::SearchContext::new(&buffer, Some(&search_settings));
        let search_entry = gtk::SearchEntry::builder().hexpand(true).placeholder_text("Find").build();
        let replace_entry = gtk::Entry::builder().hexpand(true).placeholder_text("Replace").build();
        let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let search_bar = gtk::SearchBar::builder().show_close_button(true).build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&search_bar);
        root.append(&gtk::ScrolledWindow::builder().vexpand(true).hexpand(true).child(&view).build());

        let page = Rc::new(EditorPage {
            root,
            buffer,
            view,
            vim_label: gtk::Label::builder().xalign(0.0).css_classes(["monospace", "dim-label"]).build(),
            path: RefCell::new(path.to_path_buf()),
            settings,
            lsp: RefCell::new(None),
            version: Cell::new(0),
            diagnostics: RefCell::new(Vec::new()),
            change_timer: RefCell::new(None),
            autosave_timer: RefCell::new(None),
            saved_mtime: Cell::new(None),
            on_event: RefCell::new(None),
            vim: RefCell::new(None),
            key_seq: RefCell::default(),
            mark_active: Cell::new(false),
            replicating: Cell::new(false),
            pending_insert: Cell::new(false),
            pending_delete: Cell::new(None),
            replay: RefCell::default(),
            in_user_action: Cell::new(false),
            expand_stack: RefCell::default(),
            tag_caret_sel,
            tag_error,
            tag_warning,
            tag_flash,
            search_bar,
            search_entry,
            replace_row,
            replace_entry,
            search_settings,
            search_ctx,
        });
        page.build_search();

        let completion = page.view.completion();
        completion.set_select_on_show(true);
        let cp: CompletionProvider = glib::Object::new();
        *cp.imp().page.borrow_mut() = Rc::downgrade(&page);
        completion.add_provider(&cp);
        let hp: HoverProvider = glib::Object::new();
        *hp.imp().page.borrow_mut() = Rc::downgrade(&page);
        page.view.hover().add_provider(&hp);

        page.load();

        // replicate edits made at the primary caret onto the extra carets
        let w = Rc::downgrade(&page);
        page.buffer.connect_insert_text(move |_, at, _| {
            if let Some(p) = w.upgrade() {
                p.before_insert(at);
            }
        });
        let w = Rc::downgrade(&page);
        page.buffer.connect_local("insert-text", true, move |args| {
            if let (Some(p), Ok(text)) = (w.upgrade(), args[2].get::<String>()) {
                p.after_insert(&text);
            }
            None
        });
        let w = Rc::downgrade(&page);
        page.buffer.connect_delete_range(move |_, s, e| {
            if let Some(p) = w.upgrade() {
                p.before_delete(s, e);
            }
        });
        let w = Rc::downgrade(&page);
        page.buffer.connect_local("delete-range", true, move |_| {
            if let Some(p) = w.upgrade() {
                p.after_delete();
            }
            None
        });
        let w = Rc::downgrade(&page);
        page.buffer.connect_begin_user_action(move |_| {
            if let Some(p) = w.upgrade() {
                p.in_user_action.set(true);
            }
        });
        let w = Rc::downgrade(&page);
        page.buffer.connect_end_user_action(move |_| {
            if let Some(p) = w.upgrade() {
                p.in_user_action.set(false);
                p.flush_replay();
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = Rc::downgrade(&page);
        keys.connect_key_pressed(move |_, key, code, state| match w.upgrade() {
            Some(p) => p.on_key(key, code, state),
            None => glib::Propagation::Proceed,
        });
        page.view.add_controller(keys);
        let focus = gtk::EventControllerFocus::new();
        let w = Rc::downgrade(&page);
        focus.connect_leave(move |_| {
            if let Some(p) = w.upgrade() {
                p.set_key_seq(Vec::new());
            }
        });
        page.view.add_controller(focus);

        let w = Rc::downgrade(&page);
        page.buffer.connect_changed(move |_| {
            if let Some(p) = w.upgrade() {
                p.on_changed();
            }
        });
        let w = Rc::downgrade(&page);
        page.buffer.connect_modified_changed(move |_| {
            if let Some(p) = w.upgrade() {
                p.emit(PageEvent::Modified);
            }
        });
        let w = Rc::downgrade(&page);
        page.buffer.connect_cursor_position_notify(move |_| {
            if let Some(p) = w.upgrade() {
                p.emit(PageEvent::Cursor);
            }
        });
        let w = Rc::downgrade(&page);
        page.view.connect_line_mark_activated(move |_, it, button, _, _| {
            if let (Some(p), 1) = (w.upgrade(), button) {
                p.toggle_breakpoint(Some(it.line()));
            }
        });
        let click = gtk::GestureClick::builder().button(1).propagation_phase(gtk::PropagationPhase::Capture).build();
        let w = Rc::downgrade(&page);
        click.connect_pressed(move |g, n, x, y| {
            let Some(p) = w.upgrade() else { return };
            let km = p.keymap();
            let state = g.current_event_state() & crate::keymap::MODS;
            // the gesture sits on the scrolled window so it runs before the view's own
            let Some(pt) = g.widget().and_then(|w| w.compute_point(&p.view, &gtk::graphene::Point::new(x as f32, y as f32))) else { return };
            let (x, y) = (pt.x() as f64, pt.y() as f64);
            if !p.view.contains(x, y) {
                return;
            }
            let (bx, by) = p.view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            // past the end of a line GTK reports no iter; use the line end then
            let it = p.view.iter_at_location(bx, by).unwrap_or_else(|| carets::line_end(&p.view.line_at_y(by).0));
            if n == 1 && state == km.add_cursor_click {
                g.set_state(gtk::EventSequenceState::Claimed);
                p.toggle_caret_at(&it);
            } else if state.contains(gdk::ModifierType::CONTROL_MASK) && km.ctrl_click_definition {
                p.clear_carets();
                p.buffer.place_cursor(&it);
                p.emit(PageEvent::CtrlClick);
            } else if state.is_empty() {
                p.clear_carets();
                p.mark_active.set(false);
            }
        });
        page.view.parent().expect("view is in a scrolled window").add_controller(click);
        page.apply_settings();
        page
    }

    pub fn connect_event(&self, f: impl Fn(&Rc<EditorPage>, PageEvent) + 'static) {
        *self.on_event.borrow_mut() = Some(Rc::new(f));
    }

    fn emit(self: &Rc<Self>, ev: PageEvent) {
        let cb = self.on_event.borrow().clone();
        if let Some(cb) = cb {
            cb(self, ev);
        }
    }

    // -- properties ------------------------------------------------------

    pub fn path(&self) -> PathBuf {
        self.path.borrow().clone()
    }

    pub fn set_path(&self, p: &Path) {
        *self.path.borrow_mut() = p.to_path_buf();
    }

    pub fn uri(&self) -> String {
        crate::util::path_to_uri(&self.path.borrow())
    }

    pub fn is_typst(&self) -> bool {
        is_typst(&self.path.borrow())
    }

    pub fn title(&self) -> String {
        crate::util::basename(&self.path.borrow())
    }

    pub fn modified(&self) -> bool {
        self.buffer.is_modified()
    }

    pub fn lsp(&self) -> Option<Rc<Lsp>> {
        self.lsp.borrow().clone()
    }

    pub fn text(&self) -> String {
        let (s, e) = self.buffer.bounds();
        self.buffer.text(&s, &e, true).to_string()
    }

    pub fn cursor(&self) -> gtk::TextIter {
        self.buffer.iter_at_mark(&self.buffer.get_insert())
    }

    pub fn cursor_position(&self) -> (i32, i32) {
        let it = self.cursor();
        (it.line(), it.line_offset())
    }

    // -- file io ---------------------------------------------------------

    fn load(&self) {
        let path = self.path();
        let text = std::fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        self.clear_carets();
        self.buffer.begin_irreversible_action();
        self.buffer.set_text(&text);
        self.buffer.end_irreversible_action();
        self.buffer.set_modified(false);
        self.buffer.place_cursor(&self.buffer.start_iter());
        self.saved_mtime.set(mtime(&path));
    }

    pub fn reload(&self) {
        let (line, col) = self.cursor_position();
        self.load();
        self.goto(line, col, false, None, false);
        self.flush_changes();
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = self.path();
        let tmp = path.with_extension("plars-tmp");
        std::fs::write(&tmp, self.text())?;
        std::fs::rename(&tmp, &path)?;
        self.buffer.set_modified(false);
        self.saved_mtime.set(mtime(&path));
        if let (Some(lsp), true) = (self.lsp(), self.is_typst()) {
            self.flush_changes();
            lsp.did_save(&path);
        }
        Ok(())
    }

    /// Changed on disk since we loaded/saved it? Re-arms so it reports once.
    pub fn take_disk_change(&self) -> bool {
        let now = mtime(&self.path());
        let changed = now.is_some() && self.saved_mtime.get().is_some() && now != self.saved_mtime.get();
        if changed {
            self.saved_mtime.set(now);
        }
        changed
    }

    // -- LSP sync --------------------------------------------------------

    pub fn attach_lsp(&self, lsp: Option<Rc<Lsp>>) {
        if let (Some(l), true) = (&lsp, self.is_typst()) {
            self.version.set(self.version.get() + 1);
            l.did_open(&self.path(), &self.text(), self.version.get());
        }
        *self.lsp.borrow_mut() = lsp;
    }

    pub fn detach_lsp(&self) {
        if let (Some(l), true) = (self.lsp(), self.is_typst()) {
            self.flush_changes();
            l.did_close(&self.path());
        }
        *self.lsp.borrow_mut() = None;
    }

    fn on_changed(self: &Rc<Self>) {
        if let Some(id) = self.change_timer.borrow_mut().take() {
            id.remove();
        }
        let w = Rc::downgrade(self);
        *self.change_timer.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_millis(120), move || {
            if let Some(p) = w.upgrade() {
                p.change_timer.borrow_mut().take();
                p.flush_changes();
            }
        }));
        if self.settings.get().auto_save {
            if let Some(id) = self.autosave_timer.borrow_mut().take() {
                id.remove();
            }
            let w = Rc::downgrade(self);
            *self.autosave_timer.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_secs(2), move || {
                if let Some(p) = w.upgrade() {
                    p.autosave_timer.borrow_mut().take();
                    if p.modified() {
                        let _ = p.save();
                    }
                }
            }));
        }
        self.emit(PageEvent::TextChanged);
    }

    pub fn flush_changes(&self) {
        if let Some(id) = self.change_timer.borrow_mut().take() {
            id.remove();
        }
        if let (Some(lsp), true) = (self.lsp(), self.is_typst()) {
            self.version.set(self.version.get() + 1);
            lsp.did_change(&self.path(), &self.text(), self.version.get());
        }
    }

    fn insert_completion(&self, context: &sourceview5::CompletionContext, item: &Value) {
        let buf = &self.buffer;
        let (text, (mut start, mut end)) = match item.get("textEdit") {
            Some(edit) if !edit.is_null() => {
                let range = if edit["range"].is_null() { &edit["replace"] } else { &edit["range"] };
                (edit["newText"].as_str().unwrap_or("").to_owned(), range_iters(buf, range))
            }
            _ => (
                item["insertText"].as_str().or(item["label"].as_str()).unwrap_or("").to_owned(),
                context.bounds().unwrap_or((self.cursor(), self.cursor())),
            ),
        };
        // The range was computed when the request was sent; text typed while the
        // list was open (the filter word) has to be replaced as well.
        let cursor = self.cursor();
        if cursor.line() == end.line() && cursor.offset() > end.offset() {
            end = cursor;
        }
        let snippet_fmt = item["insertTextFormat"].as_i64() == Some(2);
        if self.has_extra_carets() {
            let (before, after) = (cursor.offset() - start.offset(), end.offset() - cursor.offset());
            self.complete_at_carets(&text, snippet_fmt, before.max(0), after.max(0));
            return;
        }
        buf.begin_user_action();
        buf.delete(&mut start, &mut end);
        let snippet = snippet_fmt && text.contains('$');
        match snippet.then(|| sourceview5::Snippet::new_parsed(&text)) {
            Some(Ok(snip)) => self.view.push_snippet(&snip, Some(&mut start)),
            Some(Err(_)) => {
                let plain = regex::Regex::new(r"\$\{\d+:?([^}]*)\}|\$\d+").unwrap().replace_all(&text, "$1");
                buf.insert(&mut start, &plain);
            }
            None => buf.insert(&mut start, &text),
        }
        if let Some(extra) = item["additionalTextEdits"].as_array() {
            apply_text_edits(buf, extra);
        }
        buf.end_user_action();
    }

    // -- diagnostics -----------------------------------------------------

    pub fn set_diagnostics(&self, diags: Vec<Value>) {
        let (s, e) = self.buffer.bounds();
        self.buffer.remove_tag(&self.tag_error, &s, &e);
        self.buffer.remove_tag(&self.tag_warning, &s, &e);
        for d in &diags {
            let (s, mut e) = range_iters(&self.buffer, &d["range"]);
            if s == e {
                e.forward_char();
            }
            let tag = if d["severity"].as_i64().unwrap_or(1) == 1 { &self.tag_error } else { &self.tag_warning };
            self.buffer.apply_tag(tag, &s, &e);
        }
        *self.diagnostics.borrow_mut() = diags;
    }

    fn diagnostics_at(&self, it: &gtk::TextIter) -> Vec<Value> {
        self.diagnostics
            .borrow()
            .iter()
            .filter(|d| {
                let (s, e) = range_iters(&self.buffer, &d["range"]);
                s <= *it && *it <= e
            })
            .cloned()
            .collect()
    }

    // -- marks: breakpoints & execution line -----------------------------

    pub fn toggle_breakpoint(self: &Rc<Self>, line: Option<i32>) {
        let line = line.unwrap_or_else(|| self.cursor_position().0);
        let marks = self.buffer.source_marks_at_line(line, Some("breakpoint"));
        if marks.is_empty() {
            if let Some(it) = self.buffer.iter_at_line(line) {
                self.buffer.create_source_mark(None, "breakpoint", &it);
            }
        } else {
            for m in marks {
                self.buffer.delete_mark(&m);
            }
        }
        self.emit(PageEvent::Breakpoint);
    }

    /// 0-based lines carrying a breakpoint.
    pub fn breakpoints(&self) -> Vec<i32> {
        (0..self.buffer.line_count())
            .filter(|&l| !self.buffer.source_marks_at_line(l, Some("breakpoint")).is_empty())
            .collect()
    }

    pub fn set_breakpoints(&self, lines: &[i32]) {
        let (s, e) = self.buffer.bounds();
        self.buffer.remove_source_marks(&s, &e, Some("breakpoint"));
        for &l in lines {
            if let Some(it) = self.buffer.iter_at_line(l) {
                self.buffer.create_source_mark(None, "breakpoint", &it);
            }
        }
    }

    pub fn set_exec_line(&self, line: Option<i32>) {
        let (s, e) = self.buffer.bounds();
        self.buffer.remove_source_marks(&s, &e, Some("exec"));
        if let Some(mut it) = line.and_then(|l| self.buffer.iter_at_line(l)) {
            self.buffer.create_source_mark(None, "exec", &it);
            self.view.scroll_to_iter(&mut it, 0.25, false, 0.0, 0.0);
        }
    }

    // -- navigation ------------------------------------------------------

    pub fn goto(&self, line: i32, col: i32, scroll: bool, select_to: Option<(i32, i32)>, flash: bool) {
        let it = self
            .buffer
            .iter_at_line_offset(line, col)
            .or_else(|| self.buffer.iter_at_line(line))
            .unwrap_or_else(|| self.buffer.end_iter());
        match select_to.and_then(|(l, c)| self.buffer.iter_at_line_offset(l, c)) {
            Some(end) => self.buffer.select_range(&it, &end),
            None => self.buffer.place_cursor(&it),
        }
        if scroll {
            let view = self.view.clone();
            let mark = self.buffer.get_insert();
            glib::idle_add_local_once(move || view.scroll_to_mark(&mark, 0.2, true, 0.0, 0.3));
        }
        if flash {
            let mut s = it;
            s.set_line_offset(0);
            let mut e = s;
            e.forward_to_line_end();
            self.buffer.apply_tag(&self.tag_flash, &s, &e);
            let (buf, tag) = (self.buffer.clone(), self.tag_flash.clone());
            glib::timeout_add_local_once(Duration::from_millis(900), move || {
                let (s, e) = buf.bounds();
                buf.remove_tag(&tag, &s, &e);
            });
        }
    }

    pub fn goto_lsp(&self, line: i64, character: i64, flash: bool) {
        let it = iter_at_lsp(&self.buffer, line, character);
        self.goto(it.line(), it.line_offset(), true, None, flash);
    }

    // -- in-file search --------------------------------------------------

    fn build_search(self: &Rc<Self>) {
        let settings = self.search_settings.clone();
        self.search_entry.connect_search_changed(move |e| {
            let t = e.text();
            settings.set_search_text((!t.is_empty()).then_some(t.as_str()));
        });
        let w = Rc::downgrade(self);
        let step = move |fwd: bool| {
            let w = w.clone();
            move |_: &gtk::SearchEntry| {
                if let Some(p) = w.upgrade() {
                    p.search_step(fwd)
                }
            }
        };
        self.search_entry.connect_activate(step(true));
        self.search_entry.connect_next_match(step(true));
        self.search_entry.connect_previous_match(step(false));
        let bar = self.search_bar.clone();
        self.search_entry.connect_stop_search(move |_| bar.set_search_mode(false));

        let toggle = |label: &str, tip: &str, prop: &str| {
            let b = gtk::ToggleButton::builder().label(label).tooltip_text(tip).build();
            b.bind_property("active", &self.search_settings, prop).bidirectional().build();
            b
        };
        let count = gtk::Label::builder().css_classes(["dim-label"]).width_chars(8).build();
        let c = count.clone();
        self.search_ctx.connect_occurrences_count_notify(move |ctx| {
            let n = ctx.occurrences_count();
            c.set_text(&if n < 0 { String::new() } else { format!("{n} found") });
        });
        let row1 = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row1.append(&self.search_entry);
        row1.append(&count);
        row1.append(&toggle("Aa", "Match case", "case-sensitive"));
        row1.append(&toggle("W", "Whole word", "at-word-boundaries"));
        row1.append(&toggle(".*", "Regular expression", "regex-enabled"));
        for (icon, fwd) in [("go-up-symbolic", false), ("go-down-symbolic", true)] {
            let b = gtk::Button::from_icon_name(icon);
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    p.search_step(fwd)
                }
            });
            row1.append(&b);
        }
        let replace = gtk::Button::with_label("Replace");
        let w = Rc::downgrade(self);
        replace.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                if let Some((mut s, mut e)) = p.buffer.selection_bounds() {
                    let _ = p.search_ctx.replace(&mut s, &mut e, &p.replace_entry.text());
                }
                p.search_step(true);
            }
        });
        let replace_all = gtk::Button::with_label("Replace All");
        let w = Rc::downgrade(self);
        replace_all.connect_clicked(move |_| {
            if let Some(p) = w.upgrade() {
                let _ = p.search_ctx.replace_all(&p.replace_entry.text());
            }
        });
        self.replace_row.append(&self.replace_entry);
        self.replace_row.append(&replace);
        self.replace_row.append(&replace_all);
        let col = gtk::Box::new(gtk::Orientation::Vertical, 6);
        col.append(&row1);
        col.append(&self.replace_row);
        self.search_bar.set_child(Some(&col));
        self.search_bar.connect_entry(&self.search_entry);
    }

    pub fn show_search(&self, replace: bool) {
        self.search_bar.set_search_mode(true);
        if let Some((s, e)) = self.buffer.selection_bounds() {
            if s.line() == e.line() {
                self.search_entry.set_text(&self.buffer.text(&s, &e, true));
            }
        }
        self.replace_row.set_visible(replace);
        self.search_entry.grab_focus();
    }

    fn search_step(&self, forward: bool) {
        let found = if forward {
            let from = self.buffer.selection_bounds().map(|(_, e)| e).unwrap_or_else(|| self.cursor());
            self.search_ctx.forward(&from)
        } else {
            self.search_ctx.backward(&self.cursor())
        };
        if let Some((mut s, e, _)) = found {
            self.buffer.select_range(&s, &e);
            self.view.scroll_to_iter(&mut s, 0.2, false, 0.0, 0.0);
        }
    }

    // -- settings --------------------------------------------------------

    pub fn apply_settings(&self) {
        let s = self.settings.get().clone();
        self.view.set_show_line_numbers(s.show_line_numbers);
        self.view.set_highlight_current_line(s.highlight_current_line);
        self.view.set_wrap_mode(if s.wrap_text { gtk::WrapMode::WordChar } else { gtk::WrapMode::None });
        self.view.set_tab_width(s.tab_width);
        self.view.set_indent_width(s.tab_width as i32);
        self.view.set_insert_spaces_instead_of_tabs(true);
        self.apply_scheme();
        self.set_key_seq(Vec::new());
        self.set_vim(crate::keymap::get(&s.keymap).vim);
    }

    pub fn apply_scheme(&self) {
        let mgr = sourceview5::StyleSchemeManager::default();
        let dark = adw::StyleManager::default().is_dark();
        let id = self.settings.get().style_scheme.clone();
        let scheme = (id != "auto")
            .then(|| mgr.scheme(&id))
            .flatten()
            .or_else(|| mgr.scheme(if dark { "Adwaita-dark" } else { "Adwaita" }));
        self.buffer.set_style_scheme(scheme.as_ref());
        // extra carets' selections use the scheme's selection colour
        let sel = scheme
            .as_ref()
            .and_then(|s| s.style("selection"))
            .filter(|st| st.property::<bool>("background-set"))
            .and_then(|st| st.property::<Option<String>>("background"))
            .and_then(|c| gdk::RGBA::parse(&c).ok());
        let sel = sel.unwrap_or(gdk::RGBA::new(0.21, 0.52, 0.89, 0.30));
        self.tag_caret_sel.set_background_rgba(Some(&sel));
    }

    fn set_vim(&self, enabled: bool) {
        let mut vim = self.vim.borrow_mut();
        if enabled && vim.is_none() {
            let im = sourceview5::VimIMContext::new();
            im.set_client_widget(Some(&self.view));
            im.bind_property("command-bar-text", &self.vim_label, "label").sync_create().build();
            let buf = self.buffer.clone();
            let path = self.path.borrow().clone();
            im.connect_write(move |_, _, _| {
                let (s, e) = buf.bounds();
                if std::fs::write(&path, buf.text(&s, &e, true).as_str()).is_ok() {
                    buf.set_modified(false);
                }
            });
            let ctl = gtk::EventControllerKey::new();
            ctl.set_im_context(Some(&im));
            ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
            self.view.add_controller(ctl.clone());
            *vim = Some(ctl);
        } else if !enabled {
            if let Some(ctl) = vim.take() {
                self.view.remove_controller(&ctl);
                self.vim_label.set_label("");
            }
        }
    }
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}
