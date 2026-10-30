//! Minimal Debug Adapter Protocol client for `tinymist dap`.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use gtk::glib;
use serde_json::{json, Value};

use crate::rpc::{Event, Transport};

/// (success, body, message)
pub type Callback = Box<dyn FnOnce(bool, Value, String)>;

pub enum DapEvent {
    Event(String, Value),
    Log(String),
    Exit,
}

pub struct Dap {
    transport: Transport,
    seq: Cell<u64>,
    pending: RefCell<HashMap<u64, Callback>>,
    alive: Cell<bool>,
    handler: Box<dyn Fn(DapEvent)>,
}

impl Dap {
    pub fn start(tinymist: &str, cwd: &Path, handler: impl Fn(DapEvent) + 'static) -> std::io::Result<Rc<Dap>> {
        let (transport, rx) = Transport::spawn(&[tinymist, "dap"], cwd)?;
        let dap = Rc::new(Dap {
            transport,
            seq: Cell::new(0),
            pending: RefCell::new(HashMap::new()),
            alive: Cell::new(true),
            handler: Box::new(handler),
        });
        let weak = Rc::downgrade(&dap);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(dap) = weak.upgrade() else { break };
                dap.on_event(ev);
            }
        });
        Ok(dap)
    }

    pub fn alive(&self) -> bool {
        self.alive.get()
    }

    pub fn kill(&self) {
        self.transport.kill();
    }

    /// tinymist rejects `{}` for argument-less requests (configurationDone,
    /// threads), so `Value::Null` omits the `arguments` field entirely.
    pub fn request(&self, command: &str, args: Value, cb: impl FnOnce(bool, Value, String) + 'static) {
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        let mut msg = json!({ "seq": seq, "type": "request", "command": command });
        if !args.is_null() {
            msg["arguments"] = args;
        }
        self.pending.borrow_mut().insert(seq, Box::new(cb));
        self.transport.send(&msg);
    }

    fn on_event(&self, ev: Event) {
        match ev {
            Event::Message(mut msg) => match msg["type"].as_str() {
                Some("response") => {
                    let seq = msg["request_seq"].as_u64().unwrap_or(0);
                    let cb = self.pending.borrow_mut().remove(&seq);
                    if let Some(cb) = cb {
                        let ok = msg["success"].as_bool().unwrap_or(false);
                        let text = msg["message"].as_str().unwrap_or_default().to_owned();
                        cb(ok, msg["body"].take(), text);
                    }
                }
                Some("event") => {
                    let name = msg["event"].as_str().unwrap_or_default().to_owned();
                    (self.handler)(DapEvent::Event(name, msg["body"].take()));
                }
                Some("request") => {
                    // reverse requests (runInTerminal, ...) are not supported
                    let seq = self.seq.get() + 1;
                    self.seq.set(seq);
                    self.transport.send(&json!({ "seq": seq, "type": "response",
                        "request_seq": msg["seq"], "command": msg["command"],
                        "success": false, "message": "not supported" }));
                }
                _ => {}
            },
            Event::Stderr(line) => (self.handler)(DapEvent::Log(line)),
            Event::Exit(_) => {
                self.alive.set(false);
                let pending: Vec<Callback> = self.pending.borrow_mut().drain().map(|(_, c)| c).collect();
                for cb in pending {
                    cb(false, Value::Null, "debug adapter exited".into());
                }
                (self.handler)(DapEvent::Exit);
            }
        }
    }
}
