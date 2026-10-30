//! Plars — a GTK4 Typst editor built on tinymist
//! (language server, live preview and debug adapter).

// a GUI app on Windows: no console window
#![cfg_attr(windows, windows_subsystem = "windows")]

mod dap;
mod debugger;
mod dialogs;
mod editor;
mod keymap;
mod lsp;
mod pageview;
mod panels;
mod preview;
mod render;
mod rpc;
mod settings;
mod sidebar;
mod util;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

use settings::Settings;
use window::Window;

const APP_ID: &str = "io.github.plars.Plars";


/// Where to find `language-specs/typst.lang`: next to the executable's
/// installed share dir, the source tree (for `cargo run`), and $XDG_DATA_DIRS.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("data")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(|p| p.parent()) {
            dirs.push(prefix.join("share/plars"));
        }
    }
    dirs.push(glib::user_data_dir().join("plars"));
    dirs.extend(glib::system_data_dirs().into_iter().map(|d| d.join("plars")));
    dirs.retain(|d| d.join("language-specs").is_dir());
    dirs
}

/// Install the application accelerators of keymap `id`.
pub fn apply_accels(app: &impl IsA<gtk::Application>, id: &str) {
    for (action, accels) in keymap::get(id).accels() {
        let refs: Vec<&str> = accels.iter().map(String::as_str).collect();
        app.set_accels_for_action(&action, &refs);
    }
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE)
        .build();
    // windows keep themselves alive through GTK; we only hold them so the Rc
    // state lives as long as the widget.
    let windows: Rc<RefCell<Vec<Rc<Window>>>> = Rc::default();
    let settings: Rc<RefCell<Option<Rc<Settings>>>> = Rc::default();

    let s = settings.clone();
    app.connect_startup(move |app| {
        sourceview5::init();
        let st = Settings::load();
        *s.borrow_mut() = Some(st.clone());
        let actions: [(&str, Box<dyn Fn(&adw::Application)>); 4] = [
            ("preferences", {
                let st = st.clone();
                Box::new(move |app| crate::dialogs::preferences(&st).present(app.active_window().as_ref()))
            }),
            ("about", Box::new(|app| about().present(app.active_window().as_ref()))),
            ("quit", Box::new(|app| app.windows().iter().for_each(|w| w.close()))),
            ("new-window", Box::new(|app| app.activate())),
        ];
        for (name, f) in actions {
            let act = gio::SimpleAction::new(name, None);
            let a = app.downgrade();
            act.connect_activate(move |_, _| {
                if let Some(app) = a.upgrade() {
                    f(&app);
                }
            });
            app.add_action(&act);
        }
        apply_accels(app, &st.get().keymap);
    });

    let new_window = {
        let (windows, settings) = (windows.clone(), settings.clone());
        move |app: &adw::Application| -> Rc<Window> {
            let win = Window::new(app, settings.borrow().clone().unwrap());
            let (ws, w) = (windows.clone(), Rc::downgrade(&win));
            win.widget.connect_destroy(move |_| {
                ws.borrow_mut().retain(|x| !std::ptr::eq(Rc::as_ptr(x), w.as_ptr()));
            });
            windows.borrow_mut().push(win.clone());
            win
        }
    };

    let nw = new_window.clone();
    app.connect_activate(move |app| nw(app).widget.present());

    app.connect_open(move |app, files, _| {
        for f in files {
            let Some(path) = f.path() else { continue };
            let win = new_window(app);
            if path.is_dir() {
                win.open_folder(&path, true);
            } else {
                if !path.exists() {
                    let _ = std::fs::File::create(&path);
                }
                if let Some(dir) = path.parent() {
                    win.open_folder(dir, false);
                }
                win.open_file(&path);
            }
            win.widget.present();
        }
    });
    app.run()
}

fn about() -> adw::AboutDialog {
    adw::AboutDialog::builder()
        .application_name("Plars")
        .application_icon("accessories-text-editor")
        .version(env!("CARGO_PKG_VERSION"))
        .developer_name("Plars contributors")
        .comments("A GTK4 Typst editor with live preview, diagnostics and a debugger, all powered by \
                   tinymist.\n\nA clean-room Linux clone of the macOS app Edist, built for a university \
                   assignment.")
        .website("https://github.com/Myriad-Dreamin/tinymist")
        .license_type(gtk::License::MitX11)
        .build()
}
