//! Preferences, new-project-from-template, and the Typst documentation window.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
#[cfg(feature = "webkit")]
use webkit6::prelude::*;

use crate::settings::Settings;

/// A few popular templates on Typst Universe (https://typst.app/universe).
const TEMPLATES: &[(&str, &str)] = &[
    ("charged-ieee", "IEEE-style conference paper"),
    ("basic-resume", "Simple résumé / CV"),
    ("elsearticle", "Elsevier journal article"),
    ("diatypst", "Slides"),
    ("touying", "Presentation slides"),
    ("ilm", "Non-fiction book"),
    ("graceful-genetics", "Genetics journal article"),
    ("splendid-mdpi", "MDPI journal article"),
    ("unequivocal-ams", "AMS mathematics paper"),
    ("bamdone-rebuttal", "Paper review rebuttal"),
    ("classy-german-invoice", "Invoice (German)"),
    ("cram-snap", "Cheat sheet"),
    ("dashing-dept-news", "Department newsletter"),
    ("fireside", "Letter"),
    ("red-agora", "Report / thesis cover"),
    ("grape-suite", "Exercise sheets, seminar papers"),
];

pub fn preferences(settings: &Rc<Settings>) -> adw::PreferencesDialog {
    let dlg = adw::PreferencesDialog::builder().title("Preferences").search_enabled(true).build();
    let s = settings.get().clone();

    let editor = adw::PreferencesPage::builder().title("Editor").icon_name("document-edit-symbolic").build();
    let appearance = adw::PreferencesGroup::builder().title("Appearance").build();
    let mgr = sourceview5::StyleSchemeManager::default();
    let mut ids: Vec<String> = vec!["auto".into()];
    ids.extend(mgr.scheme_ids().iter().map(|s| s.to_string()));
    let names: Vec<String> = ids
        .iter()
        .map(|id| if id == "auto" { "Follow system (Adwaita)".into() } else { mgr.scheme(id).map(|s| s.name().to_string()).unwrap_or(id.clone()) })
        .collect();
    let names_ref: Vec<&str> = names.iter().map(String::as_str).collect();
    let scheme = adw::ComboRow::builder().title("Color scheme").model(&gtk::StringList::new(&names_ref)).build();
    scheme.set_selected(ids.iter().position(|i| *i == s.style_scheme).unwrap_or(0) as u32);
    let st = settings.clone();
    scheme.connect_selected_notify(move |r| {
        let id = ids[r.selected() as usize].clone();
        st.set("style_scheme", |d| d.style_scheme = id);
    });
    appearance.add(&scheme);

    let font_row = adw::ActionRow::builder().title("Font").build();
    let font_btn = gtk::FontDialogButton::builder()
        .dialog(&gtk::FontDialog::new())
        .valign(gtk::Align::Center)
        .use_font(true)
        .level(gtk::FontLevel::Family)
        .build();
    font_btn.set_font_desc(&gtk::pango::FontDescription::from_string(&s.font_family));
    let st = settings.clone();
    font_btn.connect_font_desc_notify(move |b| {
        if let Some(fam) = b.font_desc().and_then(|d| d.family()) {
            st.set("font_family", |d| d.font_family = fam.to_string());
        }
    });
    font_row.add_suffix(&font_btn);
    appearance.add(&font_row);
    let size = adw::SpinRow::with_range(6.0, 48.0, 1.0);
    size.set_title("Font size");
    size.set_value(s.font_size as f64);
    let st = settings.clone();
    size.connect_value_notify(move |r| st.set("font_size", |d| d.font_size = r.value() as i32));
    appearance.add(&size);
    editor.add(&appearance);

    let behaviour = adw::PreferencesGroup::builder().title("Behaviour").build();
    type Get = fn(&crate::settings::Data) -> bool;
    type Put = fn(&mut crate::settings::Data, bool);
    let keymaps = crate::keymap::KEYMAPS;
    let names: Vec<&str> = keymaps.iter().map(|k| k.name).collect();
    let km = adw::ComboRow::builder()
        .title("Keymap")
        .subtitle("Key bindings modelled on another editor")
        .model(&gtk::StringList::new(&names))
        .build();
    km.set_selected(keymaps.iter().position(|k| k.id == s.keymap).unwrap_or(0) as u32);
    let st = settings.clone();
    km.connect_selected_notify(move |r| {
        let id = keymaps[r.selected() as usize].id.to_owned();
        st.set("keymap", |d| d.keymap = id);
    });
    behaviour.add(&km);
    let switches: [(&str, &str, &str, Get, Put); 4] = [
        ("show_line_numbers", "Line numbers", "", |d| d.show_line_numbers, |d, v| d.show_line_numbers = v),
        ("highlight_current_line", "Highlight current line", "", |d| d.highlight_current_line, |d, v| d.highlight_current_line = v),
        ("wrap_text", "Wrap lines", "", |d| d.wrap_text, |d, v| d.wrap_text = v),
        ("auto_save", "Auto-save", "Save two seconds after you stop typing", |d| d.auto_save, |d, v| d.auto_save = v),
    ];
    for (key, title, subtitle, get, put) in switches {
        let row = adw::SwitchRow::builder().title(title).active(get(&s)).build();
        if !subtitle.is_empty() {
            row.set_subtitle(subtitle);
        }
        let st = settings.clone();
        row.connect_active_notify(move |r| st.set(key, |d| put(d, r.is_active())));
        behaviour.add(&row);
    }
    let tab = adw::SpinRow::with_range(1.0, 8.0, 1.0);
    tab.set_title("Indent width");
    tab.set_value(s.tab_width as f64);
    let st = settings.clone();
    tab.connect_value_notify(move |r| st.set("tab_width", |d| d.tab_width = r.value() as u32));
    behaviour.add(&tab);
    editor.add(&behaviour);
    dlg.add(&editor);

    let typst = adw::PreferencesPage::builder().title("Typst").icon_name("applications-science-symbolic").build();
    let server = adw::PreferencesGroup::builder()
        .title("tinymist")
        .description("Language server, live preview and debug adapter")
        .build();
    let path = adw::EntryRow::builder()
        .title("tinymist executable (empty = search $PATH)")
        .text(&s.tinymist_path)
        .show_apply_button(true)
        .build();
    let st = settings.clone();
    path.connect_apply(move |r| {
        let t = r.text().trim().to_owned();
        st.set("tinymist_path", |d| d.tinymist_path = t);
    });
    server.add(&path);
    typst.add(&server);
    let prev = adw::PreferencesGroup::builder().title("Preview").build();
    let modes = ["never", "auto", "always"];
    let inv = adw::ComboRow::builder()
        .title("Invert preview colours")
        .subtitle("“Auto” follows the dark style")
        .model(&gtk::StringList::new(&["Never", "Auto", "Always"]))
        .build();
    inv.set_selected(modes.iter().position(|m| *m == s.preview_invert).unwrap_or(0) as u32);
    let st = settings.clone();
    inv.connect_selected_notify(move |r| {
        let m = modes[r.selected() as usize].to_owned();
        st.set("preview_invert", |d| d.preview_invert = m);
    });
    prev.add(&inv);
    let follow = adw::SwitchRow::builder().title("Follow cursor in preview").active(s.follow_cursor).build();
    let st = settings.clone();
    follow.connect_active_notify(move |r| st.set("follow_cursor", |d| d.follow_cursor = r.is_active()));
    prev.add(&follow);
    let partial = adw::SwitchRow::builder()
        .title("Partial rendering")
        .subtitle("Only render the visible pages; smoother scrolling in long documents")
        .active(s.partial_rendering)
        .build();
    let st = settings.clone();
    partial.connect_active_notify(move |r| st.set("partial_rendering", |d| d.partial_rendering = r.is_active()));
    prev.add(&partial);
    typst.add(&prev);
    dlg.add(&typst);
    dlg
}

/// Pick a Typst Universe template, a parent folder and a name, then `typst init`
/// (or tinymist's `doInitTemplate` when the typst CLI isn't installed).
pub fn template_dialog(win: &Rc<crate::window::Window>) -> adw::Dialog {
    let dlg = adw::Dialog::builder().title("New Project from Template").content_width(520).content_height(640).build();
    let tv = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let create = gtk::Button::builder().label("Create").css_classes(["suggested-action"]).sensitive(false).build();
    header.pack_end(&create);
    tv.add_top_bar(&header);

    let page = adw::PreferencesPage::new();
    let g1 = adw::PreferencesGroup::builder()
        .title("Template")
        .description("Downloaded from Typst Universe")
        .build();
    let spec = adw::EntryRow::builder().title("Package (e.g. @preview/charged-ieee)").build();
    g1.add(&spec);
    let search = gtk::SearchEntry::builder().placeholder_text("Search templates").margin_top(6).build();
    g1.add(&search);
    let list = gtk::ListBox::builder().css_classes(["boxed-list"]).margin_top(6).build();
    let name = adw::EntryRow::builder().title("Project name").build();
    for (n, desc) in TEMPLATES {
        let row = adw::ActionRow::builder().title(*n).subtitle(*desc).activatable(true).build();
        let (spec, name) = (spec.clone(), name.clone());
        row.connect_activated(move |_| {
            spec.set_text(&format!("@preview/{n}"));
            if name.text().is_empty() {
                name.set_text(n);
            }
        });
        list.append(&row);
    }
    let s = search.clone();
    list.set_filter_func(move |row| {
        let q = s.text().to_lowercase();
        let row = row.downcast_ref::<adw::ActionRow>().unwrap();
        q.is_empty() || row.title().to_lowercase().contains(&q) || row.subtitle().is_some_and(|t| t.to_lowercase().contains(&q))
    });
    let l = list.clone();
    search.connect_search_changed(move |_| l.invalidate_filter());
    g1.add(&list);
    page.add(&g1);

    let g2 = adw::PreferencesGroup::builder().title("Location").build();
    g2.add(&name);
    let parent = Rc::new(std::cell::RefCell::new(glib::home_dir()));
    let loc = adw::ActionRow::builder().title("Create in").subtitle(parent.borrow().to_string_lossy()).activatable(true).build();
    loc.add_suffix(&gtk::Image::from_icon_name("folder-open-symbolic"));
    let (p, w) = (parent.clone(), Rc::downgrade(win));
    loc.connect_activated(move |row| {
        let (p, row) = (p.clone(), row.clone());
        let Some(win) = w.upgrade() else { return };
        gtk::FileDialog::builder().title("Choose where to create the project").build().select_folder(
            Some(&win.widget),
            gio::Cancellable::NONE,
            move |res| {
                if let Some(path) = res.ok().and_then(|f| f.path()) {
                    row.set_subtitle(&path.to_string_lossy());
                    *p.borrow_mut() = path;
                }
            },
        );
    });
    g2.add(&loc);
    page.add(&g2);
    let spinner = adw::Spinner::builder().visible(false).height_request(32).margin_bottom(12).build();
    tv.set_content(Some(&page));
    tv.add_bottom_bar(&spinner);
    dlg.set_child(Some(&tv));

    let validate = {
        let (spec, name, create) = (spec.clone(), name.clone(), create.clone());
        move || create.set_sensitive(!spec.text().trim().is_empty() && !name.text().trim().is_empty())
    };
    let v = validate.clone();
    spec.connect_changed(move |_| v());
    let v = validate.clone();
    name.connect_changed(move |_| v());

    let (w, d) = (Rc::downgrade(win), dlg.downgrade());
    create.connect_clicked(move |btn| {
        let Some(win) = w.upgrade() else { return };
        let mut spec_s = spec.text().trim().to_owned();
        if !spec_s.starts_with('@') {
            spec_s = format!("@preview/{spec_s}");
        }
        let dest = parent.borrow().join(name.text().trim());
        if dest.exists() {
            win.toast(&format!("{} already exists", dest.display()));
            return;
        }
        spinner.set_visible(true);
        btn.set_sensitive(false);
        let (spinner, btn, d, w) = (spinner.clone(), btn.clone(), d.clone(), w.clone());
        let finish = move |error: Option<String>, dest: PathBuf| {
            spinner.set_visible(false);
            btn.set_sensitive(true);
            let Some(win) = w.upgrade() else { return };
            match error {
                Some(e) => win.toast(e.trim().lines().last().unwrap_or("Failed")),
                None => {
                    if let Some(d) = d.upgrade() {
                        d.close();
                    }
                    win.open_folder(&dest, true);
                }
            }
        };
        init_template(&win, &spec_s, &dest, finish);
    });
    dlg
}

fn init_template(win: &crate::window::Window, spec: &str, dest: &Path, finish: impl Fn(Option<String>, PathBuf) + 'static) {
    let dest = dest.to_path_buf();
    let typst = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("typst")).find(|p| p.is_file()));
    if let Some(typst) = typst {
        let argv = [typst.as_os_str(), "init".as_ref(), spec.as_ref(), dest.as_os_str()];
        match gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE) {
            Ok(proc) => {
                let p = proc.clone();
                proc.communicate_utf8_async(None, gio::Cancellable::NONE, move |res| {
                    let out = res.ok().and_then(|(o, _)| o).map(|s| s.to_string()).unwrap_or_default();
                    if p.is_successful() {
                        finish(None, dest)
                    } else {
                        finish(Some(if out.is_empty() { "typst init failed".into() } else { out }), dest)
                    }
                })
            }
            Err(e) => finish(Some(e.to_string()), dest),
        }
    } else if let Some(lsp) = win.lsp() {
        lsp.command("tinymist.doInitTemplate", serde_json::json!([spec, dest]), move |r| {
            finish(r.err().map(|e| e["message"].as_str().unwrap_or("failed").to_owned()), dest)
        });
    } else {
        finish(Some("Neither typst nor tinymist is available".into()), dest);
    }
}

#[cfg(feature = "webkit")]
pub fn docs_window(parent: &impl IsA<gtk::Window>) -> adw::Window {
    let win = adw::Window::builder()
        .title("Typst Documentation")
        .default_width(1000)
        .default_height(760)
        .transient_for(parent)
        .build();
    let web = webkit6::WebView::builder().vexpand(true).build();
    let header = adw::HeaderBar::new();
    for (icon, tip, act) in [("go-previous-symbolic", "Back", 0), ("go-next-symbolic", "Forward", 1), ("go-home-symbolic", "Overview", 2)] {
        let b = gtk::Button::builder().icon_name(icon).tooltip_text(tip).build();
        let web = web.clone();
        b.connect_clicked(move |_| match act {
            0 => web.go_back(),
            1 => web.go_forward(),
            _ => web.load_uri("https://typst.app/docs/"),
        });
        header.pack_start(&b);
    }
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&web));
    win.set_content(Some(&tv));
    web.load_uri("https://typst.app/docs/");
    win
}
