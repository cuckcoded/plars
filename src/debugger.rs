//! Debugger panel driving `tinymist dap`.
//!
//! Flow: initialize -> launch{program, root} -> setBreakpoints (per file) ->
//! configurationDone; `stopped` events then trigger stackTrace -> scopes -> variables.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{json, Value};

use crate::dap::{Dap, DapEvent};
use crate::window::Window;

#[derive(Clone)]
struct VarNode {
    name: String,
    value: String,
    vtype: String,
    reference: i64,
    scope: bool,
    expensive: bool,
}

pub struct DebugPanel {
    pub root: gtk::Box,
    win: RefCell<Weak<Window>>,
    dap: RefCell<Option<Rc<Dap>>>,
    stopped: Cell<bool>,
    thread_id: Cell<i64>,
    frame_id: Cell<Option<i64>>,
    /// tinymist always pauses at document start but only reports it with
    /// `stopOnEntry: true`, so we always ask for it and skip it if unwanted.
    skip_entry_stop: Cell<bool>,
    /// Requests the adapter answered with "method not found" (tinymist 0.15
    /// has no stepping or pause yet); their buttons get disabled.
    unsupported: RefCell<std::collections::HashSet<String>>,
    frames: RefCell<Vec<Value>>,
    buttons: RefCell<Vec<(&'static str, gtk::Button)>>,
    stop_on_entry: gtk::CheckButton,
    status: gtk::Label,
    stack_list: gtk::ListBox,
    var_root: gio::ListStore,
    var_tree: RefCell<Option<gtk::TreeListModel>>,
    console: gtk::TextView,
    entry: gtk::Entry,
}

impl DebugPanel {
    pub fn new() -> Rc<DebugPanel> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let panel = Rc::new(DebugPanel {
            root,
            win: RefCell::new(Weak::new()),
            dap: RefCell::new(None),
            stopped: Cell::new(false),
            thread_id: Cell::new(1),
            frame_id: Cell::new(None),
            skip_entry_stop: Cell::new(false),
            unsupported: RefCell::default(),
            frames: RefCell::default(),
            buttons: RefCell::default(),
            stop_on_entry: gtk::CheckButton::builder().label("Stop on entry").margin_start(12).build(),
            status: gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .margin_start(12)
                .css_classes(["dim-label"])
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build(),
            stack_list: gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build(),
            var_root: gio::ListStore::new::<glib::BoxedAnyObject>(),
            var_tree: RefCell::new(None),
            console: gtk::TextView::builder()
                .editable(false)
                .monospace(true)
                .wrap_mode(gtk::WrapMode::WordChar)
                .cursor_visible(false)
                .left_margin(6)
                .build(),
            entry: gtk::Entry::builder().placeholder_text("Evaluate a Typst expression in the current frame…").build(),
        });
        panel.build();
        panel
    }

    pub fn set_window(&self, w: Weak<Window>) {
        *self.win.borrow_mut() = w;
    }

    fn win(&self) -> Option<Rc<Window>> {
        self.win.borrow().upgrade()
    }

    fn build(self: &Rc<Self>) {
        let bar = gtk::Box::builder().spacing(2).margin_start(6).margin_top(4).margin_bottom(4).build();
        type Action = fn(&Rc<DebugPanel>);
        let defs: [(&'static str, &str, &str, Action); 6] = [
            ("start", "media-playback-start-symbolic", "Start / Continue (F5)", |p| p.start_or_continue()),
            ("next", "go-next-symbolic", "Step Over (F10)", |p| p.step("next")),
            ("stepIn", "go-down-symbolic", "Step Into (F11)", |p| p.step("stepIn")),
            ("stepOut", "go-up-symbolic", "Step Out (Shift+F11)", |p| p.step("stepOut")),
            ("pause", "media-playback-pause-symbolic", "Pause", |p| p.pause()),
            ("stop", "media-playback-stop-symbolic", "Stop (Shift+F5)", |p| p.stop()),
        ];
        for (key, icon, tip, action) in defs {
            let b = gtk::Button::builder().icon_name(icon).tooltip_text(tip).css_classes(["flat"]).build();
            let w = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(p) = w.upgrade() {
                    action(&p);
                }
            });
            bar.append(&b);
            self.buttons.borrow_mut().push((key, b));
        }
        bar.append(&self.stop_on_entry);
        bar.append(&self.status);
        self.root.append(&bar);
        self.root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let w = Rc::downgrade(self);
        self.stack_list.connect_row_selected(move |_, row| {
            if let (Some(p), Some(row)) = (w.upgrade(), row) {
                p.select_frame(row.index() as usize);
            }
        });

        // variables: a lazily-loaded tree; children are fetched when a row expands
        let w = Rc::downgrade(self);
        let tree = gtk::TreeListModel::new(self.var_root.clone(), false, false, move |obj| {
            let node = obj.downcast_ref::<glib::BoxedAnyObject>()?.borrow::<VarNode>().clone();
            let p = w.upgrade()?;
            (node.reference > 0).then(|| p.fetch_children(node.reference).upcast())
        });
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            row.append(&gtk::Label::builder().xalign(0.0).css_classes(["monospace"]).build());
            row.append(&gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["monospace"])
                .build());
            row.append(&gtk::Label::builder().xalign(1.0).css_classes(["dim-label", "caption"]).margin_end(8).build());
            item.set_child(Some(&gtk::TreeExpander::builder().child(&row).build()));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else { return };
            let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
            expander.set_list_row(Some(&row));
            let Some(obj) = row.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            let node = obj.borrow::<VarNode>();
            let bx = expander.child().unwrap();
            let name = bx.first_child().and_downcast::<gtk::Label>().unwrap();
            let value = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let vtype = value.next_sibling().and_downcast::<gtk::Label>().unwrap();
            if node.scope {
                name.set_label(&node.name);
                name.set_css_classes(&["heading"]);
                value.set_label("");
                vtype.set_label(&node.value);
            } else {
                name.set_label(&format!("{}:", node.name));
                name.set_css_classes(&["monospace"]);
                value.set_label(&node.value.replace('\n', " "));
                value.set_tooltip_text(Some(&node.value));
                vtype.set_label(&node.vtype);
            }
        });
        let var_view = gtk::ListView::new(Some(gtk::NoSelection::new(Some(tree.clone()))), Some(factory));
        *self.var_tree.borrow_mut() = Some(tree);

        let w = Rc::downgrade(self);
        self.entry.connect_activate(move |e| {
            if let Some(p) = w.upgrade() {
                p.evaluate(&e.text());
                e.set_text("");
            }
        });
        let console_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        console_box.append(&gtk::ScrolledWindow::builder().vexpand(true).child(&self.console).build());
        console_box.append(&self.entry);

        let left = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&titled("Call Stack", &gtk::ScrolledWindow::builder().vexpand(true).child(&self.stack_list).build()))
            .end_child(&titled("Variables", &gtk::ScrolledWindow::builder().vexpand(true).child(&var_view).build()))
            .position(260)
            .vexpand(true)
            .build();
        let outer = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&left)
            .end_child(&titled("Debug Console", &console_box))
            .position(640)
            .build();
        self.root.append(&outer);
        self.update_buttons();
    }

    // -- session control -------------------------------------------------

    pub fn active(&self) -> bool {
        self.dap.borrow().as_ref().is_some_and(|d| d.alive())
    }

    fn dap(&self) -> Option<Rc<Dap>> {
        self.dap.borrow().clone()
    }

    pub fn start_or_continue(self: &Rc<Self>) {
        if self.active() {
            if self.stopped.get() {
                self.resume("continue");
            }
        } else {
            self.start();
        }
    }

    fn start(self: &Rc<Self>) {
        let Some(win) = self.win() else { return };
        let (Some(tinymist), Some(program)) = (win.tinymist(), win.debug_target()) else {
            self.print("No Typst file to debug (and tinymist is required).\n");
            return;
        };
        win.save_all();
        let root = win.project_root().unwrap_or_else(|| program.parent().unwrap().to_path_buf());
        self.console.buffer().set_text("");
        self.print(&format!("Launching debugger for {}\n", crate::util::relative(&program, Some(&root))));
        let w = Rc::downgrade(self);
        let dap = match Dap::start(&tinymist, &root, move |ev| {
            if let Some(p) = w.upgrade() {
                p.on_event(ev);
            }
        }) {
            Ok(d) => d,
            Err(e) => {
                self.print(&format!("could not start tinymist dap: {e}\n"));
                return;
            }
        };
        *self.dap.borrow_mut() = Some(dap.clone());
        self.skip_entry_stop.set(!self.stop_on_entry.is_active());
        dap.request("initialize", json!({
            "clientID": "plars", "adapterID": "typst", "pathFormat": "path",
            "linesStartAt1": true, "columnsStartAt1": true, "supportsVariableType": true,
        }), self.log_failure("initialize"));
        // Breakpoints sent before tinymist reaches its entry pause are silently
        // dropped, so the session is configured when the `entry` stop arrives.
        dap.request("launch", json!({
            "program": program, "root": root, "stopOnEntry": true,
        }), self.log_failure("launch"));
        self.status.set_label("Running…");
        self.update_buttons();
    }

    fn log_failure(self: &Rc<Self>, what: &'static str) -> impl FnOnce(bool, Value, String) + 'static {
        let w = Rc::downgrade(self);
        move |ok, _, msg| {
            if let (false, Some(p)) = (ok, w.upgrade()) {
                p.print(&format!("{what} failed: {msg}\n"));
            }
        }
    }

    pub fn send_breakpoints(self: &Rc<Self>, only: Option<&PathBuf>) {
        let (Some(dap), Some(win)) = (self.dap(), self.win()) else { return };
        if !dap.alive() {
            return;
        }
        for (path, lines) in win.all_breakpoints() {
            if only.is_some_and(|o| *o != path) {
                continue;
            }
            let bps: Vec<Value> = lines.iter().map(|l| json!({ "line": l + 1 })).collect();
            dap.request("setBreakpoints", json!({
                "source": { "path": path, "name": crate::util::basename(&path) },
                "breakpoints": bps,
            }), self.log_failure("setBreakpoints"));
        }
    }

    pub fn step(self: &Rc<Self>, kind: &str) {
        if self.active() && self.stopped.get() {
            self.resume(kind);
        }
    }

    fn resume(self: &Rc<Self>, kind: &str) {
        let Some(dap) = self.dap() else { return };
        self.stopped.set(false);
        if let Some(win) = self.win() {
            win.clear_exec_location();
        }
        self.status.set_label("Running…");
        self.update_buttons();
        let kind_s = kind.to_owned();
        let w = Rc::downgrade(self);
        dap.request(kind, json!({ "threadId": self.thread_id.get() }), move |ok, _, msg| {
            let (false, Some(p)) = (ok, w.upgrade()) else { return };
            if msg.contains("not found") {
                // still paused where we were
                p.unsupported.borrow_mut().insert(kind_s.clone());
                p.print(&format!("tinymist's debug adapter does not support “{kind_s}” yet — \
                                  set breakpoints and use Continue instead.\n"));
                p.stopped.set(true);
                p.status.set_label("Paused");
                p.update_buttons();
            } else {
                p.print(&format!("{kind_s}: {msg}\n"));
            }
        });
    }

    fn pause(self: &Rc<Self>) {
        if let (true, false, Some(dap)) = (self.active(), self.stopped.get(), self.dap()) {
            let w = Rc::downgrade(self);
            dap.request("pause", json!({ "threadId": self.thread_id.get() }), move |ok, _, msg| {
                let (false, Some(p)) = (ok, w.upgrade()) else { return };
                if msg.contains("not found") {
                    p.unsupported.borrow_mut().insert("pause".into());
                    p.update_buttons();
                }
                p.print(&format!("pause: {msg}\n"));
            });
        }
    }

    pub fn stop(self: &Rc<Self>) {
        let Some(dap) = self.dap() else { return };
        if !dap.alive() {
            return;
        }
        let d = dap.clone();
        dap.request("disconnect", json!({ "terminateDebuggee": true }), move |_, _, _| {
            glib::timeout_add_local_once(Duration::from_millis(300), move || d.kill());
        });
    }

    fn on_exit(&self) {
        *self.dap.borrow_mut() = None;
        self.stopped.set(false);
        if let Some(win) = self.win() {
            win.clear_exec_location();
        }
        self.stack_list.remove_all();
        self.frames.borrow_mut().clear();
        self.var_root.remove_all();
        self.status.set_label("Session ended");
        self.print("Debug session ended.\n");
        self.update_buttons();
    }

    fn update_buttons(&self) {
        let (active, stopped) = (self.active(), self.stopped.get());
        let unsupported = self.unsupported.borrow();
        for (key, b) in self.buttons.borrow().iter() {
            if unsupported.contains(*key) {
                b.set_sensitive(false);
                b.set_tooltip_text(Some("Not supported by tinymist's debug adapter yet"));
                continue;
            }
            b.set_sensitive(match *key {
                "start" => !active || stopped,
                "next" | "stepIn" | "stepOut" => active && stopped,
                "pause" => active && !stopped,
                _ => active,
            });
            if *key == "start" {
                b.set_tooltip_text(Some(if active { "Continue (F5)" } else { "Start Debugging (F5)" }));
            }
        }
        self.entry.set_sensitive(active);
        if let Some(win) = self.win() {
            win.on_debug_state(active, stopped);
        }
    }

    // -- events ----------------------------------------------------------

    fn on_event(self: &Rc<Self>, ev: DapEvent) {
        match ev {
            DapEvent::Log(line) => {
                if let Some(win) = self.win() {
                    win.log(&line);
                }
            }
            DapEvent::Exit => self.on_exit(),
            DapEvent::Event(name, body) => match name.as_str() {
                "stopped" => {
                    if body["reason"] == "entry" {
                        self.send_breakpoints(None);
                        if let Some(dap) = self.dap() {
                            dap.request("configurationDone", Value::Null, self.log_failure("configurationDone"));
                        }
                        if self.skip_entry_stop.replace(false) {
                            self.stopped.set(true);
                            self.resume("continue");
                            return;
                        }
                    }
                    self.stopped.set(true);
                    if let Some(t) = body["threadId"].as_i64() {
                        self.thread_id.set(t);
                    }
                    let reason = body["description"].as_str().or(body["reason"].as_str()).unwrap_or("").to_owned();
                    self.status.set_label(&format!("Paused: {reason}"));
                    self.print(&format!("⏸ {reason}\n"));
                    self.update_buttons();
                    let w = Rc::downgrade(self);
                    if let Some(dap) = self.dap() {
                        dap.request("stackTrace", json!({ "threadId": self.thread_id.get(), "startFrame": 0, "levels": 50 }),
                            move |ok, body, msg| {
                                if let Some(p) = w.upgrade() {
                                    p.on_stack(ok, body, msg);
                                }
                            });
                    }
                }
                "continued" => {
                    self.stopped.set(false);
                    self.update_buttons();
                }
                "output" => self.print(body["output"].as_str().unwrap_or("")),
                "exited" | "terminated" => {
                    if name == "exited" {
                        self.print(&format!("Document finished (exit code {}).\n", body["exitCode"].as_i64().unwrap_or(0)));
                    }
                    if let Some(dap) = self.dap() {
                        glib::timeout_add_local_once(Duration::from_millis(200), move || dap.kill());
                    }
                }
                _ => {}
            },
        }
    }

    fn on_stack(&self, ok: bool, body: Value, msg: String) {
        self.stack_list.remove_all();
        if !ok {
            self.print(&format!("stackTrace: {msg}\n"));
            return;
        }
        let frames = body["stackFrames"].as_array().cloned().unwrap_or_default();
        for f in &frames {
            let src = f["source"]["path"].as_str().map(PathBuf::from);
            let row = gtk::Box::builder().spacing(8).margin_start(6).margin_end(6).margin_top(2).margin_bottom(2).build();
            row.append(&gtk::Label::builder()
                .label(f["name"].as_str().unwrap_or("?"))
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .build());
            let loc = src.map(|s| format!("{}:{}", crate::util::basename(&s), f["line"].as_i64().unwrap_or(0))).unwrap_or_default();
            row.append(&gtk::Label::builder().label(loc).css_classes(["dim-label", "caption"]).build());
            self.stack_list.append(&row);
        }
        *self.frames.borrow_mut() = frames;
        if let Some(first) = self.stack_list.row_at_index(0) {
            self.stack_list.select_row(Some(&first));
        }
    }

    fn select_frame(self: &Rc<Self>, idx: usize) {
        let Some(frame) = self.frames.borrow().get(idx).cloned() else { return };
        let Some(dap) = self.dap() else { return };
        let id = frame["id"].as_i64();
        self.frame_id.set(id);
        if let (Some(src), Some(win)) = (frame["source"]["path"].as_str(), self.win()) {
            let line = (frame["line"].as_i64().unwrap_or(1) - 1).max(0) as i32;
            let col = (frame["column"].as_i64().unwrap_or(1) - 1).max(0) as i32;
            win.show_exec_location(&PathBuf::from(src), line, col);
        }
        self.var_root.remove_all();
        let w = Rc::downgrade(self);
        dap.request("scopes", json!({ "frameId": id }), move |ok, body, _| {
            let (true, Some(p)) = (ok, w.upgrade()) else { return };
            for s in body["scopes"].as_array().into_iter().flatten() {
                let count = s["namedVariables"].as_i64().map(|n| format!("{n} items")).unwrap_or_default();
                p.var_root.append(&glib::BoxedAnyObject::new(VarNode {
                    name: s["name"].as_str().unwrap_or("").into(),
                    value: count,
                    vtype: String::new(),
                    reference: s["variablesReference"].as_i64().unwrap_or(0),
                    scope: true,
                    expensive: s["expensive"].as_bool().unwrap_or(false),
                }));
            }
            // expand cheap scopes (arguments, locals); leave Globals collapsed
            let tree = p.var_tree.borrow().clone();
            if let Some(tree) = tree {
                for i in 0..tree.n_items() {
                    if let Some(row) = tree.row(i) {
                        let cheap = row
                            .item()
                            .and_downcast::<glib::BoxedAnyObject>()
                            .is_some_and(|o| !o.borrow::<VarNode>().expensive);
                        if cheap && row.depth() == 0 {
                            row.set_expanded(true);
                        }
                    }
                }
            }
        });
    }

    fn fetch_children(&self, reference: i64) -> gio::ListStore {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        if let Some(dap) = self.dap() {
            let s = store.clone();
            dap.request("variables", json!({ "variablesReference": reference }), move |ok, body, _| {
                if !ok {
                    return;
                }
                for v in body["variables"].as_array().into_iter().flatten() {
                    s.append(&glib::BoxedAnyObject::new(VarNode {
                        name: v["name"].as_str().unwrap_or("").into(),
                        value: v["value"].as_str().unwrap_or("").into(),
                        vtype: v["type"].as_str().unwrap_or("").into(),
                        reference: v["variablesReference"].as_i64().unwrap_or(0),
                        scope: false,
                        expensive: false,
                    }));
                }
            });
        }
        store
    }

    // -- console ---------------------------------------------------------

    pub fn print(&self, text: &str) {
        let buf = self.console.buffer();
        buf.insert(&mut buf.end_iter(), text);
        self.console.scroll_to_iter(&mut buf.end_iter(), 0.0, false, 0.0, 0.0);
    }

    fn evaluate(self: &Rc<Self>, expr: &str) {
        let expr = expr.trim();
        let Some(dap) = self.dap() else { return };
        if expr.is_empty() {
            return;
        }
        self.print(&format!("› {expr}\n"));
        let mut args = json!({ "expression": expr, "context": "repl" });
        if let Some(f) = self.frame_id.get() {
            args["frameId"] = json!(f);
        }
        let w = Rc::downgrade(self);
        dap.request("evaluate", args, move |ok, body, msg| {
            let Some(p) = w.upgrade() else { return };
            if ok {
                let t = body["type"].as_str().map(|t| format!("  ({t})")).unwrap_or_default();
                p.print(&format!("{}{t}\n", body["result"].as_str().unwrap_or("")));
            } else {
                p.print(&format!("error: {msg}\n"));
            }
        });
    }
}

fn titled(title: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.append(&gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .margin_start(8)
        .margin_top(4)
        .margin_bottom(2)
        .css_classes(["caption-heading"])
        .build());
    b.append(child);
    b
}
