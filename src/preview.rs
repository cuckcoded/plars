//! Live preview: tinymist's preview server rendered in a WebKitGTK view.
//!
//! `tinymist.doStartPreview` serves an HTML+WASM renderer on a local port and
//! pushes incremental updates over a websocket as the *in-memory* document
//! changes, so the preview follows unsaved edits.

use std::cell::Cell;

use adw::prelude::*;
use gtk::glib;
use webkit6::prelude::*;

pub struct PreviewPane {
    pub root: gtk::Box,
    pub webview: webkit6::WebView,
    pub lock_btn: gtk::ToggleButton,
    pub follow_btn: gtk::ToggleButton,
    pub detach_btn: gtk::Button,
    stack: gtk::Stack,
    placeholder: adw::StatusPage,
    title: gtk::Label,
    port: std::rc::Rc<Cell<u16>>,
}

impl PreviewPane {
    pub fn new(follow: bool) -> PreviewPane {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let bar = gtk::Box::builder().spacing(4).margin_start(6).margin_end(6).margin_top(4).margin_bottom(4).build();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["dim-label", "caption"])
            .build();
        let flat = |b: &gtk::Widget| b.add_css_class("flat");
        let lock_btn = gtk::ToggleButton::builder()
            .icon_name("changes-allow-symbolic")
            .tooltip_text("Lock preview to this file")
            .build();
        lock_btn.connect_toggled(|b| {
            b.set_icon_name(if b.is_active() { "changes-prevent-symbolic" } else { "changes-allow-symbolic" });
            b.set_tooltip_text(Some(if b.is_active() { "Preview locked to this file" } else { "Lock preview to this file" }));
        });
        let follow_btn = gtk::ToggleButton::builder()
            .icon_name("find-location-symbolic")
            .tooltip_text("Follow cursor in preview")
            .active(follow)
            .build();
        let reload = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Reload preview").build();
        let inspect = gtk::Button::builder()
            .icon_name("applications-engineering-symbolic")
            .tooltip_text("Web inspector (debug the preview frontend)")
            .build();
        let detach_btn = gtk::Button::builder()
            .icon_name("window-new-symbolic")
            .tooltip_text("Open preview in a separate window")
            .build();
        bar.append(&title);
        for w in [lock_btn.upcast_ref::<gtk::Widget>(), follow_btn.upcast_ref(), reload.upcast_ref(),
                  inspect.upcast_ref(), detach_btn.upcast_ref()] {
            flat(w);
            bar.append(w);
        }
        root.append(&bar);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

        let webview = webkit6::WebView::builder().vexpand(true).hexpand(true).build();
        if let Some(s) = webkit6::prelude::WebViewExt::settings(&webview) {
            s.set_enable_developer_extras(true);
            s.set_enable_back_forward_navigation_gestures(false);
        }
        let wv = webview.clone();
        reload.connect_clicked(move |_| wv.reload());
        let wv = webview.clone();
        inspect.connect_clicked(move |_| {
            if let Some(i) = wv.inspector() {
                i.show();
            }
        });

        let port = std::rc::Rc::new(Cell::new(0u16));
        let p = port.clone();
        webview.connect_decide_policy(move |view, decision, kind| {
            // keep the pane pinned to tinymist; open other links in the browser
            if kind != webkit6::PolicyDecisionType::NavigationAction {
                return false;
            }
            let Some(nav) = decision.downcast_ref::<webkit6::NavigationPolicyDecision>() else { return false };
            let uri = nav
                .navigation_action()
                .and_then(|a| a.request())
                .and_then(|r| r.uri())
                .map(|u| u.to_string())
                .unwrap_or_default();
            let local = format!("http://127.0.0.1:{}", p.get());
            if uri.starts_with("http") && !uri.starts_with(&local) {
                decision.ignore();
                let root = view.root().and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(&uri).launch(root.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
                return true;
            }
            false
        });

        let placeholder = adw::StatusPage::builder()
            .icon_name("x-office-document-symbolic")
            .title("No Preview")
            .description("Open a Typst file to see it rendered here.")
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&placeholder, Some("empty"));
        stack.add_named(&webview, Some("web"));
        root.append(&stack);

        PreviewPane { root, webview, lock_btn, follow_btn, detach_btn, stack, placeholder, title, port }
    }

    pub fn show_url(&self, port: u16, file_name: &str) {
        self.port.set(port);
        self.title.set_label(file_name);
        self.webview.load_uri(&format!("http://127.0.0.1:{port}"));
        self.stack.set_visible_child_name("web");
    }

    pub fn show_message(&self, title: &str, description: &str) {
        self.placeholder.set_title(title);
        self.placeholder.set_description(Some(&glib::markup_escape_text(description)));
        self.stack.set_visible_child_name("empty");
        self.title.set_label("");
    }
}
