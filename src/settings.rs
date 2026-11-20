//! Persistent JSON settings in $XDG_CONFIG_HOME/plars/settings.json.

use std::cell::{Ref, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Data {
    pub style_scheme: String,
    pub font_family: String,
    pub font_size: i32,
    pub show_line_numbers: bool,
    pub highlight_current_line: bool,
    pub wrap_text: bool,
    /// Legacy switch, migrated to `keymap = "vim"`.
    pub vim_mode: bool,
    pub keymap: String,
    pub auto_save: bool,
    pub tab_width: u32,
    pub tinymist_path: String,
    pub preview_invert: String,
    /// "tinymist" (its web preview) or "bitmap" (the built-in renderer).
    pub preview_renderer: String,
    pub follow_cursor: bool,
    pub partial_rendering: bool,
    pub recent_projects: Vec<String>,
}

impl Default for Data {
    fn default() -> Self {
        Data {
            style_scheme: "auto".into(),
            font_family: "Monospace".into(),
            font_size: 12,
            show_line_numbers: true,
            highlight_current_line: true,
            wrap_text: true,
            vim_mode: false,
            keymap: "default".into(),
            auto_save: false,
            tab_width: 2,
            tinymist_path: String::new(),
            preview_invert: "never".into(),
            preview_renderer: "tinymist".into(),
            follow_cursor: true,
            partial_rendering: true,
            recent_projects: Vec::new(),
        }
    }
}

type Listener = Rc<dyn Fn(&str)>;

pub struct Settings {
    path: PathBuf,
    data: RefCell<Data>,
    listeners: RefCell<Vec<Listener>>,
}

impl Settings {
    pub fn load() -> Rc<Settings> {
        let path = glib::user_config_dir().join("plars").join("settings.json");
        let mut data: Data = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if std::mem::take(&mut data.vim_mode) && data.keymap == "default" {
            data.keymap = "vim".into();
        }
        Rc::new(Settings { path, data: RefCell::new(data), listeners: RefCell::new(Vec::new()) })
    }

    pub fn get(&self) -> Ref<'_, Data> {
        self.data.borrow()
    }

    /// Mutate one setting, persist, then notify listeners with `key`.
    pub fn set(&self, key: &str, f: impl FnOnce(&mut Data)) {
        f(&mut self.data.borrow_mut());
        self.save();
        let listeners: Vec<Listener> = self.listeners.borrow().clone();
        for l in listeners {
            l(key);
        }
    }

    pub fn connect(&self, f: impl Fn(&str) + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    fn save(&self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("json.tmp");
        let s = serde_json::to_string_pretty(&*self.data.borrow()).expect("settings serialize");
        if let Err(e) = std::fs::write(&tmp, s).and_then(|_| std::fs::rename(&tmp, &self.path)) {
            eprintln!("plars: could not save settings to {}: {e}", self.path.display());
        }
    }

    pub fn add_recent(&self, folder: &str) {
        self.set("recent_projects", |d| {
            d.recent_projects.retain(|p| p != folder);
            d.recent_projects.insert(0, folder.to_owned());
            d.recent_projects.truncate(10);
        });
    }

    pub fn remove_recent(&self, folder: &str) {
        self.set("recent_projects", |d| d.recent_projects.retain(|p| p != folder));
    }
}
