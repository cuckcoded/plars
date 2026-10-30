//! Minimal LSP client for `tinymist lsp`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};

use gtk::glib;
use serde_json::{json, Value};

use crate::rpc::{Event, Transport};
use crate::util::path_to_uri;

pub type Callback = Box<dyn FnOnce(Result<Value, Value>)>;

pub enum LspEvent {
    Diagnostics(String, Vec<Value>),
    Notification(String, Value),
    ShowDocument(Value),
    Log(String),
    Ready,
    Exit(i32),
}

fn tinymist_config() -> Value {
    json!({
        "exportPdf": "never",
        "compileStatus": "enable",
        "formatterMode": "typstyle",
        "semanticTokens": "disable",
        "systemFonts": true,
        "preview": { "background": { "enabled": false } },
    })
}

enum Queued {
    Request(String, Value, Option<Callback>),
    Notify(String, Value),
}

pub struct Lsp {
    transport: Transport,
    next_id: Cell<u64>,
    pending: RefCell<HashMap<u64, Callback>>,
    queue: RefCell<Vec<Queued>>,
    ready: Cell<bool>,
    handler: Box<dyn Fn(LspEvent)>,
}

impl Lsp {
    pub fn start(tinymist: &str, root: &Path, handler: impl Fn(LspEvent) + 'static) -> std::io::Result<Rc<Lsp>> {
        let (transport, rx) = Transport::spawn(&[tinymist, "lsp"], root)?;
        let lsp = Rc::new(Lsp {
            transport,
            next_id: Cell::new(1),
            pending: RefCell::new(HashMap::new()),
            queue: RefCell::new(Vec::new()),
            ready: Cell::new(false),
            handler: Box::new(handler),
        });
        let weak: Weak<Lsp> = Rc::downgrade(&lsp);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(lsp) = weak.upgrade() else { break };
                lsp.on_event(ev);
            }
        });

        let root_uri = path_to_uri(root);
        let name = root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let weak = Rc::downgrade(&lsp);
        lsp.raw_request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "clientInfo": { "name": "plars" },
                "rootUri": root_uri,
                "workspaceFolders": [{ "uri": root_uri, "name": name }],
                "initializationOptions": tinymist_config(),
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "window": { "showDocument": { "support": true } },
                    "workspace": { "configuration": true, "workspaceFolders": true },
                    "textDocument": {
                        "synchronization": { "didSave": true },
                        "publishDiagnostics": { "relatedInformation": true },
                        "completion": { "completionItem": {
                            "snippetSupport": true,
                            "documentationFormat": ["markdown", "plaintext"] } },
                        "hover": { "contentFormat": ["markdown", "plaintext"] },
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                        "definition": { "linkSupport": true },
                        "formatting": {},
                    },
                },
            }),
            Some(Box::new(move |res| {
                let Some(lsp) = weak.upgrade() else { return };
                if let Err(e) = res {
                    (lsp.handler)(LspEvent::Log(format!("[plars] initialize failed: {e}")));
                    return;
                }
                lsp.raw_notify("initialized", json!({}));
                lsp.raw_notify("workspace/didChangeConfiguration", json!({ "settings": tinymist_config() }));
                lsp.ready.set(true);
                let queued: Vec<Queued> = lsp.queue.borrow_mut().drain(..).collect();
                for q in queued {
                    match q {
                        Queued::Request(m, p, cb) => {
                            lsp.raw_request(&m, p, cb);
                        }
                        Queued::Notify(m, p) => lsp.raw_notify(&m, p),
                    }
                }
                (lsp.handler)(LspEvent::Ready);
            })),
        );
        Ok(lsp)
    }

    pub fn stop(&self) {
        if self.ready.get() {
            self.raw_request("shutdown", Value::Null, None);
            self.raw_notify("exit", Value::Null);
        }
        self.ready.set(false);
        self.transport.kill();
    }

    // -- public API ------------------------------------------------------

    pub fn request(&self, method: &str, params: Value, cb: impl FnOnce(Result<Value, Value>) + 'static) {
        if self.ready.get() {
            self.raw_request(method, params, Some(Box::new(cb)));
        } else {
            self.queue.borrow_mut().push(Queued::Request(method.into(), params, Some(Box::new(cb))));
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        if self.ready.get() {
            self.raw_notify(method, params);
        } else {
            self.queue.borrow_mut().push(Queued::Notify(method.into(), params));
        }
    }

    pub fn command(&self, name: &str, args: Value, cb: impl FnOnce(Result<Value, Value>) + 'static) {
        self.request("workspace/executeCommand", json!({ "command": name, "arguments": args }), cb);
    }

    pub fn did_open(&self, path: &Path, text: &str, version: i64) {
        self.notify("textDocument/didOpen", json!({ "textDocument": {
            "uri": path_to_uri(path), "languageId": "typst", "version": version, "text": text } }));
    }

    /// A change without `range` replaces the whole document — valid even though
    /// tinymist advertises incremental sync.
    pub fn did_change(&self, path: &Path, text: &str, version: i64) {
        self.notify("textDocument/didChange", json!({
            "textDocument": { "uri": path_to_uri(path), "version": version },
            "contentChanges": [{ "text": text }] }));
    }

    pub fn did_save(&self, path: &Path) {
        self.notify("textDocument/didSave", json!({ "textDocument": { "uri": path_to_uri(path) } }));
    }

    pub fn did_close(&self, path: &Path) {
        self.notify("textDocument/didClose", json!({ "textDocument": { "uri": path_to_uri(path) } }));
    }

    // -- plumbing --------------------------------------------------------

    fn raw_request(&self, method: &str, params: Value, cb: Option<Callback>) {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        if let Some(cb) = cb {
            self.pending.borrow_mut().insert(id, cb);
        }
        let mut msg = json!({ "jsonrpc": "2.0", "id": id, "method": method });
        if !params.is_null() {
            msg["params"] = params;
        }
        self.transport.send(&msg);
    }

    fn raw_notify(&self, method: &str, params: Value) {
        let mut msg = json!({ "jsonrpc": "2.0", "method": method });
        if !params.is_null() {
            msg["params"] = params;
        }
        self.transport.send(&msg);
    }

    fn reply(&self, id: &Value, result: Value) {
        self.transport.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    fn on_event(&self, ev: Event) {
        match ev {
            Event::Message(msg) => self.on_message(msg),
            Event::Stderr(line) => (self.handler)(LspEvent::Log(line)),
            Event::Exit(code) => {
                self.ready.set(false);
                let pending: Vec<Callback> = self.pending.borrow_mut().drain().map(|(_, c)| c).collect();
                for cb in pending {
                    cb(Err(json!({ "message": "language server exited" })));
                }
                (self.handler)(LspEvent::Exit(code));
            }
        }
    }

    fn on_message(&self, mut msg: Value) {
        let params = msg.get_mut("params").map(Value::take).unwrap_or(Value::Null);
        let Some(method) = msg.get("method").and_then(Value::as_str).map(str::to_owned) else {
            // response
            let Some(id) = msg.get("id").and_then(Value::as_u64) else { return };
            let cb = self.pending.borrow_mut().remove(&id);
            if let Some(cb) = cb {
                match msg.get_mut("error") {
                    Some(e) if !e.is_null() => cb(Err(e.take())),
                    _ => cb(Ok(msg.get_mut("result").map(Value::take).unwrap_or(Value::Null))),
                }
            }
            return;
        };
        if let Some(id) = msg.get("id").cloned() {
            // server -> client request
            match method.as_str() {
                "workspace/configuration" => {
                    let n = params["items"].as_array().map(|a| a.len()).unwrap_or(0);
                    self.reply(&id, Value::Array(vec![tinymist_config(); n]));
                }
                "window/showDocument" => {
                    self.reply(&id, json!({ "success": true }));
                    (self.handler)(LspEvent::ShowDocument(params));
                }
                _ => self.reply(&id, Value::Null),
            }
            return;
        }
        match method.as_str() {
            "textDocument/publishDiagnostics" => {
                let uri = params["uri"].as_str().unwrap_or_default().to_owned();
                let diags = params["diagnostics"].as_array().cloned().unwrap_or_default();
                (self.handler)(LspEvent::Diagnostics(uri, diags));
            }
            "window/logMessage" | "window/showMessage" => {
                (self.handler)(LspEvent::Log(params["message"].as_str().unwrap_or_default().to_owned()))
            }
            _ => (self.handler)(LspEvent::Notification(method, params)),
        }
    }
}
