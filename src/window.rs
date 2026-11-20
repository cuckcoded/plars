//! The main editor window: tabs, sidebar, preview, bottom panel, status bar,
//! and all the glue between the editor and tinymist.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use serde_json::{json, Value};

use crate::debugger::DebugPanel;
use crate::editor::{EditorPage, PageEvent};
use crate::lsp::{Lsp, LspEvent};
use crate::panels::{LogPanel, ProblemsPanel};
use crate::preview::PreviewPane;
use crate::settings::Settings;
use crate::sidebar::{FileTree, Outline, ProjectSearch};
use crate::util::{apply_text_edits, basename, find_tinymist, is_typst, lsp_position, path_to_uri, relative, uri_to_path};

thread_local!(static FONT_CSS: gtk::CssProvider = {
    let p = gtk::CssProvider::new();
    gtk::style_context_add_provider_for_display(&gdk::Display::default().unwrap(), &p,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    p
});

fn apply_font_css(settings: &Settings) {
    let s = settings.get();
    let family = s.font_family.replace('"', "");
    FONT_CSS.with(|p| p.load_from_string(&format!(
        "textview.plars-editor {{ font-family: \"{family}\", monospace; font-size: {}pt; }}\
         .plars-status {{ padding: 2px 8px; }}\
         .plars-status button {{ min-height: 0; padding: 0 6px; }}",
        s.font_size)));
}

pub struct Window {
    pub widget: adw::ApplicationWindow,
    settings: Rc<Settings>,
    project_root: RefCell<Option<PathBuf>>,
    lsp: RefCell<Option<Rc<Lsp>>>,
    tinymist: RefCell<Option<String>>,
    pages: RefCell<Vec<Rc<EditorPage>>>,
    main_file: RefCell<Option<PathBuf>>,
    diagnostics: RefCell<HashMap<String, Vec<Value>>>,
    breakpoints: RefCell<BTreeMap<PathBuf, BTreeSet<i32>>>,
    preview_task: RefCell<Option<String>>,
    preview_file: RefCell<Option<PathBuf>>,
    /// Files reached by clicking inside the preview: they belong to the
    /// previewed document, so focusing them must not retarget the preview.
    preview_members: RefCell<BTreeSet<PathBuf>>,
    preview_seq: Cell<u32>,
    scroll_timer: RefCell<Option<glib::SourceId>>,
    scroll_pending: Cell<bool>,
    /// (file, line) the preview last jumped to; only a new line jumps again.
    last_jump: RefCell<Option<(PathBuf, i32)>>,
    outline_timer: RefCell<Option<glib::SourceId>>,
    bitmap_timer: RefCell<Option<glib::SourceId>>,
    force_close: Cell<bool>,
    detached: RefCell<Option<adw::Window>>,
    #[cfg_attr(not(feature = "webkit"), allow(dead_code))]
    docs: RefCell<Option<adw::Window>>,

    toasts: adw::ToastOverlay,
    win_title: adw::WindowTitle,
    banner: adw::Banner,
    sidebar_btn: gtk::ToggleButton,
    preview_btn: gtk::ToggleButton,
    debug_btn: gtk::Button,
    sidebar_stack: adw::ViewStack,
    tab_view: adw::TabView,
    editor_stack: gtk::Stack,
    recent_group: adw::PreferencesGroup,
    recent_rows: RefCell<Vec<adw::ActionRow>>,
    hpaned: gtk::Paned,
    bottom: gtk::Box,
    panel_stack: adw::ViewStack,
    problems_page: RefCell<Option<adw::ViewStackPage>>,
    compile_icon: gtk::Image,
    compile_label: gtk::Label,
    problems_btn: gtk::Button,
    vim_slot: gtk::Box,
    pos_label: gtk::Label,
    lang_label: gtk::Label,

    preview: PreviewPane,
    file_tree: Rc<FileTree>,
    outline: RefCell<Option<Outline>>,
    search: Rc<ProjectSearch>,
    problems: RefCell<Option<Rc<ProblemsPanel>>>,
    debug_panel: Rc<DebugPanel>,
    log_panel: Rc<LogPanel>,
}

impl Window {
    pub fn new(app: &adw::Application, settings: Rc<Settings>) -> Rc<Window> {
        let widget = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(1400)
            .default_height(880)
            .title("Plars")
            .build();
        let follow = settings.get().follow_cursor;
        let win = Rc::new(Window {
            widget,
            settings: settings.clone(),
            project_root: RefCell::default(),
            lsp: RefCell::default(),
            tinymist: RefCell::default(),
            pages: RefCell::default(),
            main_file: RefCell::default(),
            diagnostics: RefCell::default(),
            breakpoints: RefCell::default(),
            preview_task: RefCell::default(),
            preview_file: RefCell::default(),
            preview_members: RefCell::default(),
            preview_seq: Cell::new(0),
            scroll_timer: RefCell::default(),
            scroll_pending: Cell::new(false),
            last_jump: RefCell::default(),
            outline_timer: RefCell::default(),
            bitmap_timer: RefCell::default(),
            force_close: Cell::new(false),
            detached: RefCell::default(),
            docs: RefCell::default(),
            toasts: adw::ToastOverlay::new(),
            win_title: adw::WindowTitle::new("Plars", ""),
            banner: adw::Banner::builder()
                .title("tinymist was not found — install it (e.g. sudo pacman -S tinymist) or set its path in Preferences.")
                .button_label("Preferences")
                .action_name("app.preferences")
                .build(),
            sidebar_btn: gtk::ToggleButton::builder().icon_name("sidebar-show-symbolic").tooltip_text("Toggle sidebar (Ctrl+B)").build(),
            preview_btn: gtk::ToggleButton::builder().icon_name("view-dual-symbolic").active(true)
                .tooltip_text("Toggle live preview (Ctrl+Alt+P)").build(),
            debug_btn: gtk::Button::builder().icon_name("media-playback-start-symbolic")
                .tooltip_text("Debug current document (F5)").action_name("win.debug-continue").build(),
            sidebar_stack: adw::ViewStack::builder().vexpand(true).build(),
            tab_view: adw::TabView::builder().vexpand(true).build(),
            editor_stack: gtk::Stack::builder().transition_type(gtk::StackTransitionType::Crossfade).build(),
            recent_group: adw::PreferencesGroup::builder().title("Recent Projects").margin_top(18).build(),
            recent_rows: RefCell::default(),
            hpaned: gtk::Paned::builder().orientation(gtk::Orientation::Horizontal).position(720)
                .shrink_start_child(false).shrink_end_child(false).build(),
            bottom: gtk::Box::builder().orientation(gtk::Orientation::Vertical).visible(false).build(),
            panel_stack: adw::ViewStack::builder().vexpand(true).build(),
            problems_page: RefCell::default(),
            compile_icon: gtk::Image::from_icon_name("object-select-symbolic"),
            compile_label: gtk::Label::builder().label("tinymist starting…").css_classes(["caption"]).build(),
            problems_btn: gtk::Button::builder().css_classes(["flat", "caption"]).tooltip_text("Show problems").build(),
            vim_slot: gtk::Box::new(gtk::Orientation::Horizontal, 0),
            pos_label: gtk::Label::builder().css_classes(["caption", "dim-label"]).build(),
            lang_label: gtk::Label::builder().css_classes(["caption", "dim-label"]).build(),
            preview: PreviewPane::new(follow),
            file_tree: FileTree::new(),
            outline: RefCell::default(),
            search: ProjectSearch::new(),
            problems: RefCell::default(),
            debug_panel: DebugPanel::new(),
            log_panel: LogPanel::new(),
        });
        win.build_ui();
        win.setup_actions();
        apply_font_css(&settings);

        let w = Rc::downgrade(&win);
        settings.connect(move |key| {
            if let Some(win) = w.upgrade() {
                win.on_setting(key);
            }
        });
        let w = Rc::downgrade(&win);
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(win) = w.upgrade() {
                win.each_page(|p| p.apply_scheme());
                win.apply_preview_invert();
            }
        });
        let w = Rc::downgrade(&win);
        adw::StyleManager::default().connect_accent_color_notify(move |_| {
            if let Some(win) = w.upgrade() {
                win.each_page(|p| p.apply_scheme());
            }
        });
        let w = Rc::downgrade(&win);
        win.widget.connect_close_request(move |_| match w.upgrade() {
            Some(win) => win.on_close_request(),
            None => glib::Propagation::Proceed,
        });
        let w = Rc::downgrade(&win);
        win.widget.connect_is_active_notify(move |_| {
            if let Some(win) = w.upgrade() {
                win.check_disk_changes();
                // pick up images and other files edited elsewhere
                if win.widget.is_active() {
                    win.schedule_bitmap_compile();
                    win.file_tree.refresh();
                }
            }
        });
        win.apply_preview_invert();
        win.start_lsp(&glib::home_dir());
        win.update_title();
        win
    }

    fn weak(self: &Rc<Self>) -> Weak<Window> {
        Rc::downgrade(self)
    }

    // ================================================================== UI

    fn build_ui(self: &Rc<Self>) {
        let tv = adw::ToolbarView::new();
        self.toasts.set_child(Some(&tv));
        self.widget.set_content(Some(&self.toasts));

        // header -----------------------------------------------------------
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&self.win_title));
        header.pack_start(&self.sidebar_btn);
        let new_menu = gio::Menu::new();
        new_menu.append(Some("New File…"), Some("win.new-file"));
        new_menu.append(Some("New Project from Template…"), Some("win.new-project"));
        new_menu.append(Some("Open File…"), Some("win.open-file"));
        new_menu.append(Some("Open Folder…"), Some("win.open-folder"));
        header.pack_start(&gtk::MenuButton::builder().icon_name("list-add-symbolic").menu_model(&new_menu).tooltip_text("New / Open").build());

        let main_menu = gio::Menu::new();
        let section = |items: &[(&str, &str)]| {
            let m = gio::Menu::new();
            for (l, a) in items {
                m.append(Some(l), Some(a));
            }
            m
        };
        main_menu.append_section(None, &section(&[("Save", "win.save"), ("Save All", "win.save-all"),
            ("Reload from Disk", "win.reload"), ("Format Document", "win.format")]));
        main_menu.append_section(None, &section(&[("Find in Project", "win.find-project"),
            ("Toggle Bottom Panel", "win.toggle-panel"), ("Typst Documentation", "win.docs")]));
        main_menu.append_section(None, &section(&[("Restart Language Server", "win.restart-lsp"),
            ("Preferences", "app.preferences"), ("Keyboard Shortcuts", "win.shortcuts"), ("About Plars", "app.about")]));
        header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&main_menu).tooltip_text("Main menu").build());
        header.pack_end(&self.preview_btn);
        let export_menu = section(&[("Export PDF", "win.export::pdf"), ("Export PDF As…", "win.export-as"),
            ("Export PNG", "win.export::png"), ("Export SVG", "win.export::svg"),
            ("Export HTML", "win.export::html"), ("Export Markdown", "win.export::markdown")]);
        header.pack_end(&gtk::MenuButton::builder().icon_name("document-send-symbolic").menu_model(&export_menu).tooltip_text("Export").build());
        header.pack_end(&self.debug_btn);
        tv.add_top_bar(&header);
        tv.add_top_bar(&self.banner);

        let w = self.weak();
        self.preview_btn.connect_toggled(move |b| {
            if let Some(win) = w.upgrade() {
                win.set_preview_visible(b.is_active());
            }
        });

        // sidebar ----------------------------------------------------------
        self.file_tree.set_window(self.weak());
        self.search.set_window(self.weak());
        let w = self.weak();
        let outline = Outline::new(move |l, c| {
            if let Some(p) = w.upgrade().and_then(|w| w.current_page()) {
                p.goto_lsp(l, c, true);
            }
        });
        self.sidebar_stack.add_titled_with_icon(&self.file_tree.root, Some("files"), "Files", "folder-symbolic");
        self.sidebar_stack.add_titled_with_icon(&outline.root, Some("outline"), "Outline", "view-list-bullet-symbolic");
        self.sidebar_stack.add_titled_with_icon(&self.search.root, Some("search"), "Search", "system-search-symbolic");
        *self.outline.borrow_mut() = Some(outline);
        let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
        side.append(&adw::InlineViewSwitcher::builder()
            .stack(&self.sidebar_stack)
            .display_mode(adw::InlineViewSwitcherDisplayMode::Icons)
            .margin_start(6).margin_end(6).margin_top(6).margin_bottom(6)
            .build());
        side.append(&self.sidebar_stack);

        // editor area ------------------------------------------------------
        let w = self.weak();
        self.tab_view.connect_selected_page_notify(move |_| {
            if let Some(win) = w.upgrade() {
                win.on_tab_switched();
            }
        });
        let w = self.weak();
        self.tab_view.connect_close_page(move |_, tp| match w.upgrade() {
            Some(win) => win.on_close_page(tp),
            None => glib::Propagation::Proceed,
        });
        let tabs = gtk::Box::new(gtk::Orientation::Vertical, 0);
        tabs.append(&adw::TabBar::builder().view(&self.tab_view).autohide(false).build());
        tabs.append(&self.tab_view);
        self.editor_stack.add_named(&self.build_welcome(), Some("welcome"));
        self.editor_stack.add_named(&tabs, Some("tabs"));

        self.hpaned.set_start_child(Some(&self.editor_stack));
        self.hpaned.set_end_child(Some(&self.preview.root));
        let w = self.weak();
        self.preview.lock_btn.connect_toggled(move |b| {
            if let Some(win) = w.upgrade() {
                let target = if b.is_active() { win.preview_file.borrow().clone().or_else(|| win.preview_target()) } else { None };
                win.set_main_file(target, true);
            }
        });
        let w = self.weak();
        self.preview.follow_btn.connect_toggled(move |b| {
            if let Some(win) = w.upgrade() {
                if win.settings.get().follow_cursor != b.is_active() {
                    win.settings.set("follow_cursor", |d| d.follow_cursor = b.is_active());
                }
                win.last_jump.take();
                win.sync_preview_cursor();
            }
        });
        let w = self.weak();
        self.preview.detach_btn.connect_clicked(move |_| {
            if let Some(win) = w.upgrade() {
                win.toggle_detach();
            }
        });
        let w = self.weak();
        self.preview.pages.connect_jump(move |path, line, col| {
            let Some(win) = w.upgrade() else { return };
            win.preview_members.borrow_mut().insert(std::fs::canonicalize(&path).unwrap_or(path.clone()));
            if let Some(page) = win.open_file(&path) {
                page.goto(line as i32, col as i32, true, None, true);
                page.view.grab_focus();
            }
        });

        // bottom panel -----------------------------------------------------
        let w = self.weak();
        let problems = ProblemsPanel::new(move |path, l, c| {
            if let Some(win) = w.upgrade() {
                win.open_location(&path, l, c);
            }
        });
        self.debug_panel.set_window(self.weak());
        let pp = self.panel_stack.add_titled_with_icon(&problems.root, Some("problems"), "Problems", "dialog-warning-symbolic");
        self.panel_stack.add_titled_with_icon(&self.debug_panel.root, Some("debug"), "Debug", "media-playback-start-symbolic");
        self.panel_stack.add_titled_with_icon(&self.log_panel.root, Some("log"), "tinymist Log", "utilities-terminal-symbolic");
        *self.problems_page.borrow_mut() = Some(pp);
        *self.problems.borrow_mut() = Some(problems);
        let pbar = gtk::Box::builder().margin_start(6).margin_end(6).margin_top(4).margin_bottom(4).build();
        pbar.append(&adw::InlineViewSwitcher::builder().stack(&self.panel_stack).build());
        pbar.append(&gtk::Box::builder().hexpand(true).build());
        let close = gtk::Button::builder().icon_name("window-close-symbolic").css_classes(["flat"]).tooltip_text("Hide panel (Ctrl+J)").build();
        let b = self.bottom.clone();
        close.connect_clicked(move |_| b.set_visible(false));
        pbar.append(&close);
        self.bottom.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        self.bottom.append(&pbar);
        self.bottom.append(&self.panel_stack);

        let vpaned = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&self.hpaned)
            .end_child(&self.bottom)
            .resize_end_child(false)
            .shrink_end_child(false)
            .position(600)
            .build();
        let split = adw::OverlaySplitView::builder()
            .sidebar(&side)
            .content(&vpaned)
            .show_sidebar(false)
            .min_sidebar_width(220.0)
            .max_sidebar_width(340.0)
            .build();
        self.sidebar_btn.bind_property("active", &split, "show-sidebar").bidirectional().sync_create().build();
        tv.set_content(Some(&split));

        // status bar -------------------------------------------------------
        let status = gtk::Box::builder().spacing(12).css_classes(["plars-status", "toolbar"]).build();
        let w = self.weak();
        self.problems_btn.connect_clicked(move |_| {
            if let Some(win) = w.upgrade() {
                win.show_panel("problems", true);
            }
        });
        status.append(&self.compile_icon);
        status.append(&self.compile_label);
        status.append(&self.problems_btn);
        status.append(&self.vim_slot);
        status.append(&gtk::Box::builder().hexpand(true).build());
        status.append(&self.pos_label);
        status.append(&self.lang_label);
        tv.add_bottom_bar(&status);
        self.update_problem_counts();
    }

    fn build_welcome(self: &Rc<Self>) -> adw::StatusPage {
        let page = adw::StatusPage::builder()
            .icon_name("accessories-text-editor-symbolic")
            .title("Welcome to Plars")
            .description("A Typst editor for Linux, powered by tinymist")
            .build();
        let bx = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12)
            .halign(gtk::Align::Center).width_request(380).build();
        let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
        for (label, action, suggested) in [("Open Folder…", "win.open-folder", true),
            ("Open File…", "win.open-file", false), ("From Template…", "win.new-project", false)] {
            let b = gtk::Button::builder().label(label).action_name(action).css_classes(["pill"]).build();
            if suggested {
                b.add_css_class("suggested-action");
            }
            buttons.append(&b);
        }
        bx.append(&buttons);
        bx.append(&self.recent_group);
        page.set_child(Some(&bx));
        self.refresh_recent();
        page
    }

    fn refresh_recent(self: &Rc<Self>) {
        for row in self.recent_rows.borrow_mut().drain(..) {
            self.recent_group.remove(&row);
        }
        let recents: Vec<String> = self.settings.get().recent_projects.iter()
            .filter(|p| Path::new(p).is_dir()).cloned().collect();
        let mut rows = Vec::new();
        if recents.is_empty() {
            rows.push(adw::ActionRow::builder().title("No recent projects").sensitive(false).build());
        }
        let home = glib::home_dir().to_string_lossy().into_owned();
        for path in recents {
            let row = adw::ActionRow::builder()
                .title(basename(Path::new(&path)))
                .subtitle(path.replacen(&home, "~", 1))
                .activatable(true)
                .build();
            let (w, p) = (self.weak(), path.clone());
            row.connect_activated(move |_| {
                if let Some(win) = w.upgrade() {
                    win.open_folder(Path::new(&p), true);
                }
            });
            let rm = gtk::Button::builder().icon_name("edit-delete-symbolic").css_classes(["flat"])
                .valign(gtk::Align::Center).tooltip_text("Remove from recents").build();
            let w = self.weak();
            rm.connect_clicked(move |_| {
                if let Some(win) = w.upgrade() {
                    win.settings.remove_recent(&path);
                    win.refresh_recent();
                }
            });
            row.add_suffix(&rm);
            rows.push(row);
        }
        for r in &rows {
            self.recent_group.add(r);
        }
        *self.recent_rows.borrow_mut() = rows;
    }

    // ============================================================= actions

    fn setup_actions(self: &Rc<Self>) {
        let add = |name: &str, f: fn(&Rc<Window>)| {
            let act = gio::SimpleAction::new(name, None);
            let w = self.weak();
            act.connect_activate(move |_, _| {
                if let Some(win) = w.upgrade() {
                    f(&win);
                }
            });
            self.widget.add_action(&act);
        };
        add("open-folder", |w| w.choose_folder());
        add("open-file", |w| w.choose_file());
        add("new-file", |w| {
            let dir = w.default_new_dir();
            w.new_entry(dir, false)
        });
        add("new-project", |w| crate::dialogs::template_dialog(w).present(Some(&w.widget)));
        add("save", |w| {
            if let Some(p) = w.current_page() {
                w.save_page(&p)
            }
        });
        add("save-all", |w| w.save_all());
        add("reload", |w| {
            if let Some(p) = w.current_page() {
                p.reload()
            }
        });
        add("close-tab", |w| {
            if let Some(tp) = w.tab_view.selected_page() {
                w.tab_view.close_page(&tp);
            }
        });
        add("find", |w| {
            if let Some(p) = w.current_page() {
                p.show_search(false)
            }
        });
        add("replace", |w| {
            if let Some(p) = w.current_page() {
                p.show_search(true)
            }
        });
        add("find-project", |w| w.show_project_search());
        add("format", |w| w.format_document());
        add("goto-definition", |w| w.goto_definition());
        add("toggle-sidebar", |w| w.sidebar_btn.set_active(!w.sidebar_btn.is_active()));
        add("toggle-panel", |w| w.bottom.set_visible(!w.bottom.is_visible()));
        add("toggle-preview", |w| w.preview_btn.set_active(!w.preview_btn.is_active()));
        add("export-as", |w| w.export_pdf_as());
        add("docs", |w| w.show_docs());
        add("restart-lsp", |w| {
            let root = w.project_root().unwrap_or_else(glib::home_dir);
            w.start_lsp(&root)
        });
        add("zoom-in", |w| w.settings.set("font_size", |d| d.font_size = (d.font_size + 1).min(48)));
        add("zoom-out", |w| w.settings.set("font_size", |d| d.font_size = (d.font_size - 1).max(6)));
        add("toggle-breakpoint", |w| {
            if let Some(p) = w.current_page() {
                p.toggle_breakpoint(None)
            }
        });
        add("debug-continue", |w| {
            w.show_panel("debug", false);
            w.debug_panel.start_or_continue();
        });
        add("debug-stop", |w| w.debug_panel.stop());
        add("debug-next", |w| w.debug_panel.step("next"));
        add("debug-in", |w| w.debug_panel.step("stepIn"));
        add("debug-out", |w| w.debug_panel.step("stepOut"));
        add("shortcuts", |w| w.show_shortcuts());
        add("next-tab", |w| w.cycle_tab(true));
        add("prev-tab", |w| w.cycle_tab(false));

        let export = gio::SimpleAction::new("export", Some(glib::VariantTy::STRING));
        let w = self.weak();
        export.connect_activate(move |_, v| {
            if let (Some(win), Some(kind)) = (w.upgrade(), v.and_then(|v| v.get::<String>())) {
                win.export(&kind, None);
            }
        });
        self.widget.add_action(&export);
    }

    fn cycle_tab(&self, forward: bool) {
        let moved = if forward { self.tab_view.select_next_page() } else { self.tab_view.select_previous_page() };
        let n = self.tab_view.n_pages();
        if !moved && n > 0 {
            self.tab_view.set_selected_page(&self.tab_view.nth_page(if forward { 0 } else { n - 1 }));
        }
    }

    // ============================================================== LSP

    pub fn lsp(&self) -> Option<Rc<Lsp>> {
        self.lsp.borrow().clone()
    }

    pub fn tinymist(&self) -> Option<String> {
        self.tinymist.borrow().clone()
    }

    pub fn project_root(&self) -> Option<PathBuf> {
        self.project_root.borrow().clone()
    }

    fn problems(&self) -> Rc<ProblemsPanel> {
        self.problems.borrow().clone().unwrap()
    }

    fn start_lsp(self: &Rc<Self>, root: &Path) {
        if let Some(old) = self.lsp.borrow_mut().take() {
            self.each_page(|p| p.detach_lsp());
            self.kill_preview_with(&old);
            old.stop();
        }
        self.diagnostics.borrow_mut().clear();
        self.problems().clear();
        self.update_problem_counts();
        let tinymist = find_tinymist(&self.settings.get().tinymist_path);
        *self.tinymist.borrow_mut() = tinymist.clone();
        self.banner.set_revealed(tinymist.is_none());
        let Some(tinymist) = tinymist else {
            self.compile_label.set_label("tinymist not found");
            self.compile_icon.set_icon_name(Some("dialog-warning-symbolic"));
            if self.bitmap_preview() {
                self.update_preview();
            } else {
                self.preview.show_message("No Preview",
                    "tinymist is required for its live preview. Install it, or turn on the built-in renderer in Preferences.");
            }
            return;
        };
        let w = self.weak();
        match Lsp::start(&tinymist, root, move |ev| {
            if let Some(win) = w.upgrade() {
                win.on_lsp_event(ev);
            }
        }) {
            Ok(lsp) => {
                *self.lsp.borrow_mut() = Some(lsp.clone());
                self.compile_label.set_label("tinymist starting…");
                self.each_page(|p| p.attach_lsp(Some(lsp.clone())));
            }
            Err(e) => self.toast(&format!("Could not start tinymist: {e}")),
        }
    }

    fn on_lsp_event(self: &Rc<Self>, ev: LspEvent) {
        match ev {
            LspEvent::Ready => {
                self.compile_label.set_label("Ready");
                let main = self.main_file.borrow().clone();
                if let (Some(main), Some(lsp)) = (main, self.lsp()) {
                    lsp.command("tinymist.pinMain", json!([main]), |_| {});
                }
                self.update_preview();
                self.refresh_outline();
            }
            LspEvent::Exit(code) => {
                self.lsp.borrow_mut().take();
                self.preview_task.borrow_mut().take();
                self.compile_icon.set_icon_name(Some("dialog-error-symbolic"));
                self.compile_label.set_label(&format!("tinymist exited ({code}) — restart it from the menu"));
                self.log(&format!("[plars] tinymist exited with status {code}"));
            }
            LspEvent::Log(line) => self.log(&line),
            LspEvent::Diagnostics(uri, diags) => {
                self.problems().set(&uri, &diags);
                if let Some(page) = uri_to_path(&uri).and_then(|p| self.find_page(&p)) {
                    page.set_diagnostics(diags.clone());
                }
                self.diagnostics.borrow_mut().insert(uri, diags);
                self.update_problem_counts();
            }
            LspEvent::ShowDocument(params) => self.on_show_document(&params),
            LspEvent::Notification(method, params) => self.on_notification(&method, &params),
        }
    }

    pub fn log(&self, line: &str) {
        if std::env::var_os("PLARS_DEBUG").is_some() {
            eprintln!("{line}");
        }
        self.log_panel.append(line);
    }

    fn update_problem_counts(&self) {
        let (errors, warnings) = self.problems().counts();
        // tinymist 0.15 doesn't send tinymist/compileStatus to generic clients,
        // so the status follows diagnostics once the server is up
        if self.lsp().is_some() && self.compile_label.label() != "tinymist starting…" {
            self.compile_icon.set_icon_name(Some(if errors > 0 { "dialog-error-symbolic" } else { "object-select-symbolic" }));
            self.compile_label.set_label(if errors > 0 { "Compile error" } else { "Compiled" });
        }
        self.problems_btn.set_label(&format!("✖ {errors}   ⚠ {warnings}"));
        if let Some(page) = self.problems_page.borrow().as_ref() {
            let n = errors + warnings;
            page.set_title(Some(&if n > 0 { format!("Problems ({n})") } else { "Problems".into() }));
            page.set_needs_attention(errors > 0);
        }
    }

    fn on_notification(self: &Rc<Self>, method: &str, params: &Value) {
        match method {
            "tinymist/compileStatus" => match params["status"].as_str() {
                Some("Compiling") => {
                    self.compile_icon.set_icon_name(Some("content-loading-symbolic"));
                    self.compile_label.set_label("Compiling…");
                }
                Some("CompileSuccess") => {
                    self.compile_icon.set_icon_name(Some("object-select-symbolic"));
                    let mut bits = vec!["Compiled".to_owned()];
                    if let Some(n) = params["pageCount"].as_u64() {
                        bits.push(format!("{n} page{}", if n == 1 { "" } else { "s" }));
                    }
                    if let Some(n) = params["wordsCount"]["wordsCount"].as_u64() {
                        bits.push(format!("{n} words"));
                    }
                    self.compile_label.set_label(&bits.join(" · "));
                }
                Some("CompileError") => {
                    self.compile_icon.set_icon_name(Some("dialog-error-symbolic"));
                    self.compile_label.set_label("Compile error");
                }
                _ => {}
            },
            "tinymist/preview/scrollSource" => self.jump_from_preview(params),
            "tinymist/preview/dispose" => {
                let mut task = self.preview_task.borrow_mut();
                if task.as_deref() == params["taskId"].as_str() {
                    task.take();
                }
            }
            other => self.log(&format!("[plars] notification {other}: {params}")),
        }
    }

    fn on_show_document(self: &Rc<Self>, params: &Value) {
        let uri = params["uri"].as_str().unwrap_or_default();
        match uri_to_path(uri).filter(|p| p.is_file()) {
            Some(path) => {
                // tinymist uses showDocument for clicks in the preview
                self.preview_members.borrow_mut().insert(std::fs::canonicalize(&path).unwrap_or(path.clone()));
                if let Some(page) = self.open_file(&path) {
                    if let Some(start) = params["selection"].get("start") {
                        let (l, c) = crate::util::pos_of(start);
                        page.goto_lsp(l, c, true);
                    }
                }
            }
            None if params["external"].as_bool() == Some(true) => {
                gtk::UriLauncher::new(uri).launch(Some(&self.widget), gio::Cancellable::NONE, |_| {});
            }
            None => {}
        }
    }

    /// Click in the preview -> put the caret at the matching source location.
    fn jump_from_preview(self: &Rc<Self>, params: &Value) {
        let path = params["filepath"].as_str().map(PathBuf::from)
            .or_else(|| params["uri"].as_str().and_then(uri_to_path));
        let start = &params["start"];
        let (Some(path), false) = (path, start.is_null()) else { return };
        let (line, col) = match start {
            Value::Array(a) => (a.first().and_then(Value::as_i64).unwrap_or(0), a.get(1).and_then(Value::as_i64).unwrap_or(0)),
            v => crate::util::pos_of(v),
        };
        self.preview_members.borrow_mut().insert(std::fs::canonicalize(&path).unwrap_or(path.clone()));
        if let Some(page) = self.open_file(&path) {
            page.goto_lsp(line, col, true);
            page.view.grab_focus();
        }
    }

    // ============================================================== files

    fn choose_folder(self: &Rc<Self>) {
        let w = self.weak();
        gtk::FileDialog::builder().title("Choose a Typst project folder").build()
            .select_folder(Some(&self.widget), gio::Cancellable::NONE, move |res| {
                if let (Some(win), Some(p)) = (w.upgrade(), res.ok().and_then(|f| f.path())) {
                    win.open_folder(&p, true);
                }
            });
    }

    fn choose_file(self: &Rc<Self>) {
        let typ = gtk::FileFilter::new();
        typ.set_name(Some("Typst documents"));
        typ.add_pattern("*.typ");
        let any = gtk::FileFilter::new();
        any.set_name(Some("All files"));
        any.add_pattern("*");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&typ);
        filters.append(&any);
        let dlg = gtk::FileDialog::builder().title("Open File").filters(&filters).build();
        if let Some(root) = self.project_root() {
            dlg.set_initial_folder(Some(&gio::File::for_path(root)));
        }
        let w = self.weak();
        dlg.open(Some(&self.widget), gio::Cancellable::NONE, move |res| {
            let (Some(win), Some(path)) = (w.upgrade(), res.ok().and_then(|f| f.path())) else { return };
            if win.project_root().is_none() {
                win.open_folder(path.parent().unwrap(), false);
            }
            win.open_file(&path);
        });
    }

    pub fn open_folder(self: &Rc<Self>, path: &Path, open_main: bool) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if self.project_root().as_deref() == Some(&path) {
            return;
        }
        *self.project_root.borrow_mut() = Some(path.clone());
        *self.problems().project_root.borrow_mut() = Some(path.clone());
        self.settings.add_recent(&path.to_string_lossy());
        self.refresh_recent();
        self.file_tree.set_root(&path);
        self.sidebar_btn.set_active(true);
        self.main_file.borrow_mut().take();
        self.preview.lock_btn.set_active(false);
        self.start_lsp(&path);
        self.update_title();
        if open_main && self.current_page().is_none() {
            if let Some(main) = guess_main(&path) {
                self.open_file(&main);
            }
        }
    }

    pub fn find_page(&self, path: &Path) -> Option<Rc<EditorPage>> {
        self.pages.borrow().iter().find(|p| p.path() == path).cloned()
    }

    fn page_for_tab(&self, tp: &adw::TabPage) -> Option<Rc<EditorPage>> {
        let child = tp.child();
        self.pages.borrow().iter().find(|p| p.root.upcast_ref::<gtk::Widget>() == &child).cloned()
    }

    fn tab_of(&self, page: &EditorPage) -> adw::TabPage {
        self.tab_view.page(&page.root)
    }

    pub fn current_page(&self) -> Option<Rc<EditorPage>> {
        self.tab_view.selected_page().and_then(|tp| self.page_for_tab(&tp))
    }

    fn each_page(&self, mut f: impl FnMut(&Rc<EditorPage>)) {
        let pages = self.pages.borrow().clone();
        for p in &pages {
            f(p);
        }
    }

    pub fn open_file(self: &Rc<Self>, path: &Path) -> Option<Rc<EditorPage>> {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(page) = self.find_page(&path) {
            self.tab_view.set_selected_page(&self.tab_of(&page));
            return Some(page);
        }
        if !path.is_file() {
            self.toast(&format!("File not found: {}", path.display()));
            return None;
        }
        if !std::fs::read(&path).is_ok_and(|b| std::str::from_utf8(&b).is_ok()) {
            self.toast(&format!("{} is not a text file", basename(&path)));
            return None;
        }
        let page = EditorPage::new(&path, self.settings.clone());
        let bps: Vec<i32> = self.breakpoints.borrow().get(&path).map(|s| s.iter().copied().collect()).unwrap_or_default();
        page.set_breakpoints(&bps);
        if let Some(d) = self.diagnostics.borrow().get(&path_to_uri(&path)) {
            page.set_diagnostics(d.clone());
        }
        page.attach_lsp(self.lsp());
        let w = self.weak();
        page.connect_event(move |page, ev| {
            if let Some(win) = w.upgrade() {
                win.on_page_event(page, ev);
            }
        });
        self.pages.borrow_mut().push(page.clone());
        let tp = self.tab_view.append(&page.root);
        tp.set_title(&page.title());
        tp.set_tooltip(&path.to_string_lossy());
        tp.set_icon(Some(&gio::ThemedIcon::new("text-x-generic-symbolic")));
        self.editor_stack.set_visible_child_name("tabs");
        self.tab_view.set_selected_page(&tp);
        let v = page.view.clone();
        glib::idle_add_local_once(move || {
            v.grab_focus();
        });
        Some(page)
    }

    fn open_location(self: &Rc<Self>, path: &Path, line: i64, col: i64) {
        if let Some(page) = self.open_file(path) {
            page.goto_lsp(line, col, true);
            page.view.grab_focus();
        }
    }

    pub fn open_and_select(self: &Rc<Self>, path: &Path, line: i32, start: i32, end: i32) {
        if let Some(page) = self.open_file(path) {
            page.goto(line, start, true, Some((line, end)), false);
            page.view.grab_focus();
        }
    }

    fn save_page(&self, page: &EditorPage) {
        if let Err(e) = page.save() {
            self.toast(&format!("Could not save {}: {e}", page.title()));
        }
    }

    pub fn save_all(&self) {
        self.each_page(|p| {
            if p.modified() {
                self.save_page(p);
            }
        });
    }

    fn on_page_event(self: &Rc<Self>, page: &Rc<EditorPage>, ev: PageEvent) {
        match ev {
            PageEvent::Modified => {
                self.tab_of(page).set_title(&format!("{}{}", if page.modified() { "• " } else { "" }, page.title()));
                self.update_title();
            }
            PageEvent::Cursor => self.on_cursor_moved(page),
            PageEvent::Save => self.save_page(page),
            PageEvent::TextChanged => {
                self.schedule_outline();
                self.schedule_bitmap_compile();
            }
            PageEvent::Breakpoint => {
                let path = page.path();
                self.breakpoints.borrow_mut().insert(path.clone(), page.breakpoints().into_iter().collect());
                self.debug_panel.send_breakpoints(Some(&path));
            }
            PageEvent::CtrlClick => self.goto_definition(),
        }
    }

    fn on_tab_switched(self: &Rc<Self>) {
        let page = self.current_page();
        self.editor_stack.set_visible_child_name(if page.is_some() { "tabs" } else { "welcome" });
        while let Some(c) = self.vim_slot.first_child() {
            self.vim_slot.remove(&c);
        }
        match &page {
            Some(p) => {
                if p.vim_label.parent().is_some() {
                    p.vim_label.unparent();
                }
                self.vim_slot.append(&p.vim_label);
                self.lang_label.set_label(&sourceview5::prelude::BufferExt::language(&p.buffer).map(|l| l.name().to_string()).unwrap_or("Plain Text".into()));
                self.on_cursor_moved(p);
            }
            None => {
                self.pos_label.set_label("");
                self.lang_label.set_label("");
            }
        }
        self.update_title();
        self.file_tree.reveal(page.as_ref().map(|p| p.path()).as_deref());
        self.update_preview();
        self.refresh_outline();
    }

    fn on_close_page(self: &Rc<Self>, tp: &adw::TabPage) -> glib::Propagation {
        let Some(page) = self.page_for_tab(tp) else { return glib::Propagation::Proceed };
        let (w, tp2, p2) = (self.weak(), tp.clone(), page.clone());
        let finish = move |confirm: bool| {
            let Some(win) = w.upgrade() else { return };
            if confirm {
                p2.detach_lsp();
                let bps = p2.breakpoints();
                if !bps.is_empty() {
                    win.breakpoints.borrow_mut().insert(p2.path(), bps.into_iter().collect());
                }
                win.pages.borrow_mut().retain(|p| !Rc::ptr_eq(p, &p2));
            }
            win.tab_view.close_page_finish(&tp2, confirm);
        };
        if page.modified() {
            self.confirm_unsaved(vec![page], finish);
        } else {
            finish(true);
        }
        glib::Propagation::Stop
    }

    fn confirm_unsaved(self: &Rc<Self>, pages: Vec<Rc<EditorPage>>, done: impl Fn(bool) + 'static) {
        let names: Vec<String> = pages.iter().map(|p| p.title()).collect();
        let dlg = adw::AlertDialog::new(
            Some("Save Changes?"),
            Some(&format!("{} {} unsaved changes.", names.join(", "), if pages.len() == 1 { "has" } else { "have" })),
        );
        dlg.add_responses(&[("cancel", "Cancel"), ("discard", "Discard"), ("save", "Save")]);
        dlg.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dlg.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dlg.set_default_response(Some("save"));
        dlg.set_close_response("cancel");
        let w = self.weak();
        dlg.connect_response(None, move |_, r| {
            let Some(win) = w.upgrade() else { return };
            match r {
                "save" => {
                    for p in &pages {
                        win.save_page(p);
                    }
                    done(!pages.iter().any(|p| p.modified()));
                }
                "discard" => done(true),
                _ => done(false),
            }
        });
        dlg.present(Some(&self.widget));
    }

    fn on_close_request(self: &Rc<Self>) -> glib::Propagation {
        let dirty: Vec<Rc<EditorPage>> = self.pages.borrow().iter().filter(|p| p.modified()).cloned().collect();
        if !dirty.is_empty() && !self.force_close.get() {
            let w = self.weak();
            self.confirm_unsaved(dirty, move |ok| {
                if let (true, Some(win)) = (ok, w.upgrade()) {
                    win.force_close.set(true);
                    win.widget.close();
                }
            });
            return glib::Propagation::Stop;
        }
        self.debug_panel.stop();
        if let Some(lsp) = self.lsp.borrow_mut().take() {
            self.kill_preview_with(&lsp);
            lsp.stop();
        }
        let detached = self.detached.borrow_mut().take();
        if let Some(d) = detached {
            d.destroy();
        }
        glib::Propagation::Proceed
    }

    fn check_disk_changes(self: &Rc<Self>) {
        if !self.widget.is_active() {
            return;
        }
        self.each_page(|page| {
            if !page.take_disk_change() {
                return;
            }
            if !page.modified() {
                page.reload();
            } else {
                let toast = adw::Toast::builder().title(format!("{} changed on disk", page.title()))
                    .button_label("Reload").timeout(8).build();
                let p = page.clone();
                toast.connect_button_clicked(move |_| p.reload());
                self.toasts.add_toast(toast);
            }
        });
    }

    fn default_new_dir(&self) -> Option<PathBuf> {
        let root = self.project_root()?;
        match self.current_page() {
            Some(p) if p.path().starts_with(&root) => p.path().parent().map(Path::to_path_buf),
            _ => Some(root),
        }
    }

    fn ask_name(self: &Rc<Self>, heading: &str, initial: &str, done: impl Fn(String) + 'static) {
        let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
        let dlg = adw::AlertDialog::builder().heading(heading).extra_child(&entry).build();
        dlg.add_responses(&[("cancel", "Cancel"), ("ok", "OK")]);
        dlg.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        dlg.set_default_response(Some("ok"));
        dlg.set_close_response("cancel");
        let e = entry.clone();
        dlg.connect_response(None, move |_, r| {
            let t = e.text().trim().to_owned();
            if r == "ok" && !t.is_empty() {
                done(t);
            }
        });
        dlg.present(Some(&self.widget));
        let stem = initial.rsplit_once('.').map(|(s, _)| s).unwrap_or(initial).chars().count() as i32;
        glib::idle_add_local_once(move || {
            entry.grab_focus();
            entry.select_region(0, stem);
        });
    }

    fn new_entry(self: &Rc<Self>, dir: Option<PathBuf>, folder: bool) {
        let Some(dir) = dir else {
            self.toast("Open a project folder first");
            self.choose_folder();
            return;
        };
        let w = self.weak();
        self.ask_name(if folder { "New Folder" } else { "New File" }, if folder { "folder" } else { "untitled.typ" }, move |name| {
            let Some(win) = w.upgrade() else { return };
            let path = dir.join(&name);
            if path.exists() {
                win.toast(&format!("{name} already exists"));
                return;
            }
            let res = if folder {
                std::fs::create_dir_all(&path)
            } else {
                path.parent().map(std::fs::create_dir_all).transpose().and_then(|_| std::fs::File::create_new(&path).map(|_| ()))
            };
            match res {
                Ok(()) if !folder => {
                    win.open_file(&path);
                }
                Ok(()) => win.file_tree.reveal(Some(&path)),
                Err(e) => win.toast(&e.to_string()),
            }
        });
    }

    pub fn tree_action(self: &Rc<Self>, action: &str, path: &Path) {
        match action {
            "new-file" => self.new_entry(Some(path.to_path_buf()), false),
            "new-folder" => self.new_entry(Some(path.to_path_buf()), true),
            "set-main" => self.set_main_file(Some(path.to_path_buf()), false),
            "reveal" => gtk::FileLauncher::new(Some(&gio::File::for_path(path)))
                .open_containing_folder(Some(&self.widget), gio::Cancellable::NONE, |_| {}),
            "trash" => {
                if let Err(e) = gio::File::for_path(path).trash(gio::Cancellable::NONE) {
                    self.toast(e.message());
                    return;
                }
                if let Some(page) = self.find_page(path) {
                    page.buffer.set_modified(false);
                    self.tab_view.close_page(&self.tab_of(&page));
                }
            }
            "rename" => {
                let (w, old) = (self.weak(), path.to_path_buf());
                self.ask_name("Rename", &basename(path), move |name| {
                    let Some(win) = w.upgrade() else { return };
                    let new = old.with_file_name(&name);
                    if new.exists() {
                        win.toast(&format!("{name} already exists"));
                        return;
                    }
                    if let Err(e) = std::fs::rename(&old, &new) {
                        win.toast(&e.to_string());
                        return;
                    }
                    // the renamed file, or every open file inside a renamed folder
                    let moved: Vec<_> = win.pages.borrow().iter().filter(|p| p.path().starts_with(&old)).cloned().collect();
                    for page in moved {
                        let path = page.path();
                        let rest = path.strip_prefix(&old).expect("filtered by starts_with");
                        let to = if rest.as_os_str().is_empty() { new.clone() } else { new.join(rest) };
                        let lsp = page.lsp();
                        page.detach_lsp();
                        page.set_path(&to);
                        page.attach_lsp(lsp);
                        let tp = win.tab_of(&page);
                        tp.set_title(&page.title());
                        tp.set_tooltip(&to.to_string_lossy());
                    }
                    win.update_title();
                    if win.current_page().is_some_and(|p| p.path() == new) {
                        win.file_tree.reveal(Some(&new));
                    }
                });
            }
            _ => {}
        }
    }

    // ============================================================ editing

    fn on_cursor_moved(self: &Rc<Self>, page: &Rc<EditorPage>) {
        if !self.current_page().is_some_and(|c| Rc::ptr_eq(&c, page)) {
            return;
        }
        let (line, col) = page.cursor_position();
        self.pos_label.set_label(&format!("Ln {}, Col {}", line + 1, col + 1));
        // Throttle rather than debounce: send right away, then at most once per
        // interval while the caret keeps moving, so the highlight tracks it.
        if self.scroll_timer.borrow().is_some() {
            self.scroll_pending.set(true);
            return;
        }
        self.sync_preview_cursor();
        self.arm_scroll_timer();
    }

    fn arm_scroll_timer(self: &Rc<Self>) {
        let w = self.weak();
        *self.scroll_timer.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_millis(40), move || {
            let Some(win) = w.upgrade() else { return };
            win.scroll_timer.borrow_mut().take();
            if win.scroll_pending.replace(false) {
                win.sync_preview_cursor();
                win.arm_scroll_timer();
            }
        }));
    }

    fn sync_preview_cursor(&self) {
        let Some(page) = self.current_page() else { return };
        if !page.is_typst() || !self.preview.follow_btn.is_active() || !self.preview_btn.is_active() {
            return;
        }
        let bitmap = self.bitmap_preview();
        if bitmap && self.preview_file.borrow().is_none() {
            return;
        }
        let task = self.preview_task.borrow().clone();
        let (lsp, task) = match (self.lsp(), task) {
            (Some(lsp), Some(task)) => (Some(lsp), Some(task)),
            _ if bitmap => (None, None),
            _ => return,
        };
        // jump (and flash the element in the preview) only when the caret reaches another line
        let here = (page.path(), page.cursor().line());
        if self.last_jump.borrow().as_ref() == Some(&here) {
            return;
        }
        *self.last_jump.borrow_mut() = Some(here);
        if bitmap {
            let it = page.cursor();
            self.preview.pages.sync_cursor(&page.path(), it.line() as usize, it.line_offset() as usize);
            return;
        }
        let (Some(lsp), Some(task)) = (lsp, task) else { return };
        // tinymist maps the position against the text it has, so send pending edits first
        page.flush_changes();
        let pos = lsp_position(&page.buffer, &page.cursor());
        // (tinymist 0.15 ignores "changeCursorPosition"; panelScrollTo also highlights)
        lsp.command("tinymist.scrollPreview", json!([task, {
            "event": "panelScrollTo", "filepath": page.path(),
            "line": pos["line"], "character": pos["character"] }]), |_| {});
    }

    fn format_document(self: &Rc<Self>) {
        let (Some(page), Some(lsp)) = (self.current_page(), self.lsp()) else { return };
        if !page.is_typst() {
            return;
        }
        page.flush_changes();
        let (line, _) = page.cursor_position();
        let tab = self.settings.get().tab_width;
        let w = self.weak();
        lsp.request("textDocument/formatting", json!({
            "textDocument": { "uri": page.uri() }, "options": { "tabSize": tab, "insertSpaces": true } }),
            move |res| match res {
                Err(e) => {
                    if let Some(win) = w.upgrade() {
                        win.toast(&format!("Format failed: {}", e["message"].as_str().unwrap_or("")));
                    }
                }
                Ok(Value::Array(edits)) if !edits.is_empty() => {
                    apply_text_edits(&page.buffer, &edits);
                    page.goto(line.min(page.buffer.line_count() - 1), 0, false, None, false);
                }
                _ => {}
            });
    }

    fn goto_definition(self: &Rc<Self>) {
        let (Some(page), Some(lsp)) = (self.current_page(), self.lsp()) else { return };
        if !page.is_typst() {
            return;
        }
        page.flush_changes();
        let pos = lsp_position(&page.buffer, &page.cursor());
        let w = self.weak();
        lsp.request("textDocument/definition", json!({ "textDocument": { "uri": page.uri() }, "position": pos }), move |res| {
            let Ok(result) = res else { return };
            let loc = match &result {
                Value::Array(a) => a.first().cloned().unwrap_or(Value::Null),
                v => v.clone(),
            };
            let uri = loc["targetUri"].as_str().or(loc["uri"].as_str());
            let range = if loc["targetSelectionRange"].is_null() { &loc["range"] } else { &loc["targetSelectionRange"] };
            if let (Some(win), Some(path), false) = (w.upgrade(), uri.and_then(uri_to_path), range.is_null()) {
                let (l, c) = crate::util::pos_of(&range["start"]);
                win.open_location(&path, l, c);
            }
        });
    }

    fn show_project_search(&self) {
        self.sidebar_btn.set_active(true);
        self.sidebar_stack.set_visible_child_name("search");
        if let Some((s, e)) = self.current_page().and_then(|p| p.buffer.selection_bounds()) {
            if s.line() == e.line() {
                self.search.entry.set_text(&s.buffer().text(&s, &e, true));
            }
        }
        self.search.entry.grab_focus();
    }

    fn schedule_outline(self: &Rc<Self>) {
        if let Some(id) = self.outline_timer.borrow_mut().take() {
            id.remove();
        }
        let w = self.weak();
        *self.outline_timer.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_millis(600), move || {
            if let Some(win) = w.upgrade() {
                win.outline_timer.borrow_mut().take();
                win.refresh_outline();
            }
        }));
    }

    fn refresh_outline(self: &Rc<Self>) {
        let page = self.current_page().filter(|p| p.is_typst());
        let (Some(page), Some(lsp)) = (page, self.lsp()) else {
            if let Some(o) = self.outline.borrow().as_ref() {
                o.set_symbols(&[]);
            }
            return;
        };
        page.flush_changes();
        let w = self.weak();
        lsp.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": page.uri() } }), move |res| {
            let Some(win) = w.upgrade() else { return };
            if !win.current_page().is_some_and(|c| Rc::ptr_eq(&c, &page)) {
                return;
            }
            let symbols = res.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
            let outline = win.outline.borrow();
            if let Some(o) = outline.as_ref() {
                o.set_symbols(&symbols);
            }
        });
    }

    // ============================================================ preview

    fn set_preview_visible(self: &Rc<Self>, visible: bool) {
        let detached = self.detached.borrow().clone();
        match detached {
            Some(d) => d.set_visible(visible),
            None => self.preview.root.set_visible(visible),
        }
        if visible {
            self.update_preview();
        } else if let Some(lsp) = self.lsp() {
            // keep the document: shown again, the preview picks up where it was
            self.stop_preview_task(&lsp);
        }
    }

    fn preview_target(&self) -> Option<PathBuf> {
        let current = self.current_page().filter(|p| p.is_typst()).map(|p| p.path());
        let shown = self.preview_file.borrow().clone();
        if shown.is_some() && current.as_ref().is_some_and(|c| self.preview_members.borrow().contains(c)) {
            return shown;
        }
        self.main_file.borrow().clone()
            .or(current)
            .or_else(|| self.preview_file.borrow().clone()) // keep showing the last document
    }

    fn bitmap_preview(&self) -> bool {
        self.settings.get().preview_renderer == "bitmap"
    }

    fn update_preview(self: &Rc<Self>) {
        if !self.preview_btn.is_active() {
            return;
        }
        if self.bitmap_preview() {
            let Some(target) = self.preview_target() else {
                self.preview.show_message("No Preview", "Open a Typst file to see it rendered here.");
                return;
            };
            if self.preview_file.borrow().as_ref() != Some(&target) {
                self.preview_members.borrow_mut().clear();
                self.preview.pages.clear();
                self.last_jump.take();
                *self.preview_file.borrow_mut() = Some(target.clone());
            }
            self.preview.show_bitmap(&basename(&target));
            self.compile_bitmap();
            return;
        }
        let Some(lsp) = self.lsp() else { return };
        let Some(target) = self.preview_target() else {
            self.preview.show_message("No Preview", "Open a Typst file to see it rendered here.");
            return;
        };
        let same_doc = self.preview_file.borrow().as_ref() == Some(&target);
        if same_doc && self.preview_task.borrow().is_some() {
            return;
        }
        self.stop_preview_task(&lsp);
        if !same_doc {
            self.preview_members.borrow_mut().clear();
        }
        self.preview_seq.set(self.preview_seq.get() + 1);
        let task = format!("plars-{}-{}", std::process::id(), self.preview_seq.get());
        *self.preview_task.borrow_mut() = Some(task.clone());
        *self.preview_file.borrow_mut() = Some(target.clone());
        let invert = self.settings.get().preview_invert.clone();
        let args = json!([["--task-id", task, "--data-plane-host=127.0.0.1:0",
                           format!("--invert-colors={invert}"),
                           format!("--partial-rendering={}", self.settings.get().partial_rendering), target]]);
        let w = self.weak();
        lsp.command("tinymist.doStartPreview", args, move |res| {
            let Some(win) = w.upgrade() else { return };
            if win.preview_task.borrow().as_deref() != Some(task.as_str()) {
                return; // superseded
            }
            match res {
                Ok(r) if r["staticServerPort"].is_u64() => {
                    win.preview.show_url(r["staticServerPort"].as_u64().unwrap() as u16, &basename(&target));
                    let w = win.weak();
                    glib::timeout_add_local_once(Duration::from_millis(800), move || {
                        if let Some(win) = w.upgrade() {
                            win.last_jump.take();
                            win.sync_preview_cursor();
                        }
                    });
                }
                other => {
                    win.preview_task.borrow_mut().take();
                    let msg = other.err().and_then(|e| e["message"].as_str().map(String::from)).unwrap_or("unknown error".into());
                    win.preview.show_message("Preview failed", &msg);
                }
            }
        });
    }

    /// Compile for the built-in preview, with the unsaved buffers.
    fn compile_bitmap(&self) {
        let Some(main) = self.preview_file.borrow().clone() else { return };
        let root = self.project_root()
            .filter(|r| main.starts_with(r))
            .or_else(|| main.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let overlay = self.pages.borrow().iter()
            .filter(|p| p.modified())
            .map(|p| (p.path(), p.text()))
            .collect();
        self.preview.pages.compile(&root, &main, overlay);
    }

    /// Recompile shortly, coalescing bursts of keystrokes.
    fn schedule_bitmap_compile(self: &Rc<Self>) {
        if !self.bitmap_preview() || !self.preview_btn.is_active() || self.bitmap_timer.borrow().is_some() {
            return;
        }
        let w = self.weak();
        *self.bitmap_timer.borrow_mut() = Some(glib::timeout_add_local_once(Duration::from_millis(30), move || {
            if let Some(win) = w.upgrade() {
                win.bitmap_timer.borrow_mut().take();
                win.compile_bitmap();
            }
        }));
    }

    fn apply_preview_invert(&self) {
        let invert = match self.settings.get().preview_invert.as_str() {
            "always" => true,
            "auto" => adw::StyleManager::default().is_dark(),
            _ => false,
        };
        self.preview.pages.set_inverted(invert);
    }

    /// Stop tinymist's preview server; the preview keeps its document.
    fn stop_preview_task(&self, lsp: &Lsp) {
        if let Some(task) = self.preview_task.borrow_mut().take() {
            lsp.command("tinymist.doKillPreview", json!([task]), |_| {});
        }
    }

    /// Stop the preview and forget its document (and the files in it).
    fn kill_preview_with(&self, lsp: &Lsp) {
        self.stop_preview_task(lsp);
        self.preview_file.borrow_mut().take();
        self.preview_members.borrow_mut().clear();
    }

    fn set_main_file(self: &Rc<Self>, path: Option<PathBuf>, from_button: bool) {
        *self.main_file.borrow_mut() = path.clone();
        if !from_button && self.preview.lock_btn.is_active() != path.is_some() {
            self.preview.lock_btn.set_active(path.is_some()); // re-enters with from_button=true
            return;
        }
        if let Some(lsp) = self.lsp() {
            lsp.command("tinymist.pinMain", json!([path]), |_| {});
        }
        if let Some(p) = &path {
            self.toast(&format!("Main file: {}", basename(p)));
        }
        self.update_preview();
    }

    /// Move the preview into its own window, or back. Reparenting runs from an
    /// idle callback: doing it inside the button's click handler leaves GTK
    /// with pointer state pointing at the old window.
    fn toggle_detach(self: &Rc<Self>) {
        let w = self.weak();
        glib::idle_add_local_once(move || {
            let Some(me) = w.upgrade() else { return };
            let detached = me.detached.borrow().clone();
            match detached {
                Some(win) => me.reattach_preview(&win),
                None => me.detach_preview(),
            }
        });
    }

    fn detach_preview(self: &Rc<Self>) {
        self.hpaned.set_end_child(gtk::Widget::NONE);
        let win = adw::Window::builder().title("Preview — Plars").default_width(800).default_height(1000).build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&self.preview.root));
        win.set_content(Some(&tv));
        self.preview.root.set_visible(true);
        self.preview.detach_btn.set_icon_name("view-dual-symbolic");
        self.preview.detach_btn.set_tooltip_text(Some("Return preview to the main window"));
        let w = self.weak();
        win.connect_close_request(move |win| {
            // keep the window until the preview has been moved out of it
            let (w, win) = (w.clone(), win.clone());
            glib::idle_add_local_once(move || match w.upgrade() {
                Some(me) => me.reattach_preview(&win),
                None => win.destroy(),
            });
            glib::Propagation::Stop
        });
        *self.detached.borrow_mut() = Some(win.clone());
        win.present();
    }

    fn reattach_preview(&self, win: &adw::Window) {
        if self.detached.borrow_mut().take().is_none() {
            return; // already reattached
        }
        if let Some(tv) = win.content().and_downcast::<adw::ToolbarView>() {
            tv.set_content(gtk::Widget::NONE);
        }
        self.hpaned.set_end_child(Some(&self.preview.root));
        self.preview.root.set_visible(self.preview_btn.is_active());
        self.preview.detach_btn.set_icon_name("window-new-symbolic");
        self.preview.detach_btn.set_tooltip_text(Some("Open preview in a separate window"));
        win.destroy();
    }

    // ============================================================= export

    pub fn debug_target(&self) -> Option<PathBuf> {
        self.main_file.borrow().clone()
            .or_else(|| self.current_page().filter(|p| p.is_typst()).map(|p| p.path()))
            .or_else(|| self.preview_file.borrow().clone())
    }

    fn export(self: &Rc<Self>, kind: &str, then: Option<Box<dyn Fn(PathBuf)>>) {
        let (Some(target), Some(lsp)) = (self.debug_target(), self.lsp()) else {
            self.toast("Nothing to export — open a Typst file");
            return;
        };
        self.each_page(|p| p.flush_changes());
        let opts = if matches!(kind, "png" | "svg") { json!({ "page": { "merged": { "gap": "12pt" } } }) } else { json!({}) };
        let (cmd, ext) = match kind {
            "png" => ("tinymist.exportPng", "png"),
            "svg" => ("tinymist.exportSvg", "svg"),
            "html" => ("tinymist.exportHtml", "html"),
            "markdown" => ("tinymist.exportMarkdown", "md"),
            _ => ("tinymist.exportPdf", "pdf"),
        };
        let w = self.weak();
        lsp.command(cmd, json!([target, opts]), move |res| {
            let Some(win) = w.upgrade() else { return };
            match res {
                Err(e) => {
                    let msg = e["message"].as_str().unwrap_or("");
                    let short = msg.rsplit("document is not available for export:").next().unwrap_or(msg)
                        .trim().trim_matches('"').split("\\n").next().unwrap_or("").to_owned();
                    win.toast(&format!("Export failed — {}", if short.is_empty() { "see Problems".into() } else { short }));
                }
                Ok(r) => {
                    let out = find_output(&r).map(PathBuf::from).unwrap_or_else(|| target.with_extension(ext));
                    if let Some(then) = then {
                        then(out);
                        return;
                    }
                    let toast = adw::Toast::builder().title(format!("Exported {}", basename(&out)))
                        .button_label("Open").timeout(6).build();
                    let w2 = win.weak();
                    toast.connect_button_clicked(move |_| {
                        if let Some(win) = w2.upgrade() {
                            gtk::FileLauncher::new(Some(&gio::File::for_path(&out)))
                                .launch(Some(&win.widget), gio::Cancellable::NONE, |_| {});
                        }
                    });
                    win.toasts.add_toast(toast);
                }
            }
        });
    }

    fn export_pdf_as(self: &Rc<Self>) {
        let Some(target) = self.debug_target() else {
            self.toast("Nothing to export — open a Typst file");
            return;
        };
        let name = format!("{}.pdf", target.file_stem().unwrap_or_default().to_string_lossy());
        let w = self.weak();
        gtk::FileDialog::builder().title("Export PDF As").initial_name(name).build()
            .save(Some(&self.widget), gio::Cancellable::NONE, move |res| {
                let (Some(win), Some(dest)) = (w.upgrade(), res.ok().and_then(|f| f.path())) else { return };
                let w2 = win.weak();
                win.export("pdf", Some(Box::new(move |out: PathBuf| {
                    let Some(win) = w2.upgrade() else { return };
                    match if out != dest { std::fs::copy(&out, &dest).map(|_| ()) } else { Ok(()) } {
                        Ok(()) => win.toast(&format!("Exported {}", basename(&dest))),
                        Err(e) => win.toast(&e.to_string()),
                    }
                })));
            });
    }

    // ============================================================== debug

    pub fn all_breakpoints(&self) -> BTreeMap<PathBuf, Vec<i32>> {
        let mut out: BTreeMap<PathBuf, Vec<i32>> =
            self.breakpoints.borrow().iter().map(|(p, s)| (p.clone(), s.iter().copied().collect())).collect();
        self.each_page(|p| {
            out.insert(p.path(), p.breakpoints());
        });
        out
    }

    pub fn show_exec_location(self: &Rc<Self>, path: &Path, line: i32, col: i32) {
        self.clear_exec_location();
        if let Some(page) = self.open_file(path) {
            page.set_exec_line(Some(line));
            page.goto(line, col, false, None, false);
        }
    }

    pub fn clear_exec_location(&self) {
        self.each_page(|p| p.set_exec_line(None));
    }

    pub fn on_debug_state(&self, active: bool, stopped: bool) {
        self.debug_btn.set_sensitive(!active || stopped);
        self.debug_btn.set_tooltip_text(Some(if active { "Continue (F5)" } else { "Debug current document (F5)" }));
        if active {
            self.debug_btn.add_css_class("suggested-action");
        } else {
            self.debug_btn.remove_css_class("suggested-action");
        }
    }

    // =============================================================== misc

    fn show_panel(&self, name: &str, toggle: bool) {
        if toggle && self.bottom.is_visible() && self.panel_stack.visible_child_name().as_deref() == Some(name) {
            self.bottom.set_visible(false);
            return;
        }
        self.bottom.set_visible(true);
        self.panel_stack.set_visible_child_name(name);
    }

    #[cfg(not(feature = "webkit"))]
    fn show_docs(self: &Rc<Self>) {
        gtk::UriLauncher::new("https://typst.app/docs/").launch(Some(&self.widget), gio::Cancellable::NONE, |_| {});
    }

    #[cfg(feature = "webkit")]
    fn show_docs(self: &Rc<Self>) {
        let existing = self.docs.borrow().clone();
        let docs = existing.unwrap_or_else(|| {
            let d = crate::dialogs::docs_window(&self.widget);
            let w = self.weak();
            d.connect_close_request(move |_| {
                if let Some(win) = w.upgrade() {
                    win.docs.borrow_mut().take();
                }
                glib::Propagation::Proceed
            });
            *self.docs.borrow_mut() = Some(d.clone());
            d
        });
        docs.present();
    }

    fn show_shortcuts(&self) {
        let km = crate::keymap::get(&self.settings.get().keymap);
        let row = |keys: &str, what: &str| {
            let row = adw::ActionRow::builder().title(what).build();
            row.add_suffix(&gtk::Label::builder().label(keys).css_classes(["dim-label", "monospace"]).build());
            row
        };
        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_bottom(12);

        let editor = adw::PreferencesGroup::builder()
            .title("Editor")
            .description(format!("{} keymap — change it in Preferences", km.name))
            .build();
        let m = km.add_cursor_click;
        let click: String = [(gdk::ModifierType::CONTROL_MASK, "Ctrl+"), (gdk::ModifierType::META_MASK, "⌘+"),
                             (gdk::ModifierType::SHIFT_MASK, "Shift+"), (gdk::ModifierType::ALT_MASK, "Alt+")]
            .iter()
            .filter(|(f, _)| m.contains(*f))
            .map(|(_, l)| *l)
            .collect();
        editor.add(&row(&format!("{click}Click"), "Add / remove caret"));
        editor.add(&row("Escape", "Remove extra carets"));
        editor.add(&row("Ctrl+Space", "Completion"));
        let app = adw::PreferencesGroup::builder().title("Application").build();
        for (keys, cmd) in km.bindings() {
            let what = crate::keymap::describe(cmd);
            if cmd.starts_with("win.") || cmd.starts_with("app.") {
                if keys.contains(' ') {
                    app.add(&row(&keys, what));
                }
            } else {
                editor.add(&row(&keys, what));
            }
        }
        for (action, accels) in km.accels() {
            if accels.is_empty() {
                continue;
            }
            let keys: Vec<String> = accels.iter().map(|a| crate::keymap::label_chord(a)).collect();
            app.add(&row(&keys.join(" / "), crate::keymap::describe(&action)));
        }
        content.append(&editor);
        content.append(&app);

        let dlg = adw::Dialog::builder().title("Keyboard Shortcuts").content_width(520).content_height(640).build();
        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&gtk::ScrolledWindow::builder().child(&content).propagate_natural_height(true).build()));
        dlg.set_child(Some(&tv));
        dlg.present(Some(&self.widget));
    }

    pub fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::builder().title(glib::markup_escape_text(text)).timeout(4).build());
    }

    fn update_title(&self) {
        let root = self.project_root();
        let project = root.as_deref().map(basename).unwrap_or_else(|| "Plars".into());
        let page = self.current_page();
        let sub = page.as_ref()
            .map(|p| format!("{}{}", if p.modified() { "• " } else { "" }, relative(&p.path(), root.as_deref())))
            .unwrap_or_default();
        self.win_title.set_title(&project);
        self.win_title.set_subtitle(&sub);
        self.widget.set_title(Some(&match &page {
            Some(p) => format!("{} — {project}", p.title()),
            None => project,
        }));
    }

    fn on_setting(self: &Rc<Self>, key: &str) {
        match key {
            "font_family" | "font_size" => apply_font_css(&self.settings),
            "tinymist_path" => {
                let root = self.project_root().unwrap_or_else(glib::home_dir);
                self.start_lsp(&root);
            }
            "preview_renderer" => {
                if let Some(lsp) = self.lsp() {
                    self.stop_preview_task(&lsp);
                }
                // same document, other renderer; drop pages of an earlier one
                self.preview.pages.clear();
                self.last_jump.take();
                self.update_preview();
                self.sync_preview_cursor();
            }
            "preview_invert" | "partial_rendering" => {
                self.apply_preview_invert();
                if let Some(lsp) = self.lsp() {
                    self.stop_preview_task(&lsp);
                }
                self.update_preview();
            }
            "keymap" => {
                if let Some(app) = self.widget.application() {
                    crate::apply_accels(&app, &self.settings.get().keymap);
                }
                self.each_page(|p| p.apply_settings());
            }
            "follow_cursor" => {
                let f = self.settings.get().follow_cursor;
                if self.preview.follow_btn.is_active() != f {
                    self.preview.follow_btn.set_active(f);
                }
            }
            "recent_projects" => {}
            _ => self.each_page(|p| p.apply_settings()),
        }
    }
}

fn guess_main(root: &Path) -> Option<PathBuf> {
    // a Typst Universe template package points at its entrypoint
    if let Ok(text) = std::fs::read_to_string(root.join("typst.toml")) {
        if let Some(entry) = text.parse::<toml::Table>().ok()
            .and_then(|t| t.get("template")?.get("entrypoint")?.as_str().map(String::from)) {
            for base in [root.join("template"), root.to_path_buf()] {
                if base.join(&entry).is_file() {
                    return Some(base.join(entry));
                }
            }
        }
    }
    for name in ["main.typ", "index.typ", "document.typ", "report.typ", "paper.typ"] {
        if root.join(name).is_file() {
            return Some(root.join(name));
        }
    }
    let mut typs: Vec<PathBuf> = std::fs::read_dir(root).ok()?.flatten().map(|e| e.path()).filter(|p| is_typst(p)).collect();
    typs.sort();
    typs.into_iter().next()
}

fn find_output(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => ["path", "output", "file"].iter()
            .find_map(|k| o.get(*k).and_then(Value::as_str).map(String::from))
            .or_else(|| o.values().find_map(find_output)),
        Value::Array(a) => a.iter().find_map(find_output),
        _ => None,
    }
}
