//! Live preview: tinymist's preview server rendered in a WebKitGTK view
//! (or, built without the `webkit` feature, opened in the web browser).
//!
//! `tinymist.doStartPreview` serves an HTML+WASM renderer on a local port and
//! pushes incremental updates over a websocket as the *in-memory* document
//! changes, so the preview follows unsaved edits.

use std::cell::Cell;

use adw::prelude::*;
use gtk::glib;
#[cfg(feature = "webkit")]
use webkit6::prelude::*;

pub struct PreviewPane {
    pub root: gtk::Box,
    #[cfg(feature = "webkit")]
    pub webview: webkit6::WebView,
    #[cfg(not(feature = "webkit"))]
    browser: adw::StatusPage,
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

        let port = std::rc::Rc::new(Cell::new(0u16));
        #[cfg(feature = "webkit")]
        let webview = {
            let webview = webkit6::WebView::builder().vexpand(true).hexpand(true).build();
            if let Some(s) = webkit6::prelude::WebViewExt::settings(&webview) {
                s.set_enable_developer_extras(true);
                s.set_enable_back_forward_navigation_gestures(false);
            }
            if let Some(ucm) = webview.user_content_manager() {
                ucm.add_script(&webkit6::UserScript::new(
                    JUMP_HIGHLIGHT_JS,
                    webkit6::UserContentInjectedFrames::TopFrame,
                    webkit6::UserScriptInjectionTime::End,
                    &[],
                    &[],
                ));
            }
            let wv = webview.clone();
            reload.connect_clicked(move |_| wv.reload());
            let wv = webview.clone();
            inspect.connect_clicked(move |_| {
                if let Some(i) = wv.inspector() {
                    i.show();
                }
            });

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
            webview
        };
        #[cfg(not(feature = "webkit"))]
        let browser = {
            inspect.set_visible(false);
            let open = gtk::Button::builder()
                .label("Open in Browser")
                .halign(gtk::Align::Center)
                .css_classes(["pill", "suggested-action"])
                .build();
            let p = port.clone();
            let launch = move |b: &gtk::Button| open_in_browser(b, p.get());
            open.connect_clicked(launch.clone());
            reload.connect_clicked(launch);
            adw::StatusPage::builder()
                .icon_name("web-browser-symbolic")
                .title("Preview in Browser")
                .description("This build has no embedded web view, so the live preview opens in your browser. It still follows the caret.")
                .child(&open)
                .build()
        };
        let placeholder = adw::StatusPage::builder()
            .icon_name("x-office-document-symbolic")
            .title("No Preview")
            .description("Open a Typst file to see it rendered here.")
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&placeholder, Some("empty"));
        #[cfg(feature = "webkit")]
        stack.add_named(&webview, Some("web"));
        #[cfg(not(feature = "webkit"))]
        stack.add_named(&browser, Some("web"));
        root.append(&stack);
        PreviewPane {
            root,
            #[cfg(feature = "webkit")]
            webview,
            #[cfg(not(feature = "webkit"))]
            browser,
            lock_btn,
            follow_btn,
            detach_btn,
            stack,
            placeholder,
            title,
            port,
        }
    }

    pub fn show_url(&self, port: u16, file_name: &str) {
        let first = self.port.replace(port) == 0;
        self.title.set_label(file_name);
        #[cfg(feature = "webkit")]
        self.webview.load_uri(&format!("http://127.0.0.1:{port}"));
        #[cfg(not(feature = "webkit"))]
        if first {
            // open the first preview automatically; later ones via the button
            open_in_browser(&self.browser, port);
        }
        let _ = first;
        self.stack.set_visible_child_name("web");
    }

    pub fn show_message(&self, title: &str, description: &str) {
        self.placeholder.set_title(title);
        self.placeholder.set_description(Some(&glib::markup_escape_text(description)));
        self.stack.set_visible_child_name("empty");
        self.title.set_label("");
    }
}

#[cfg(not(feature = "webkit"))]
fn open_in_browser(w: &impl IsA<gtk::Widget>, port: u16) {
    if port != 0 {
        let root = w.root().and_downcast::<gtk::Window>();
        gtk::UriLauncher::new(&format!("http://127.0.0.1:{port}")).launch(root.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
    }
}

/// Injected into tinymist's preview page. When the editor makes the preview
/// jump, tinymist drops a `.typst-jump-ripple` at the target; this finds the
/// rendered text under it and flashes a highlight over that element.
#[cfg(feature = "webkit")]
const JUMP_HIGHLIGHT_JS: &str = r#"
(() => {
  const style = document.createElement('style');
  style.textContent = `
    .plars-jump-flash {
      position: absolute; pointer-events: none; z-index: 10;
      border-radius: 3px; background: rgba(255, 196, 0, 0.38);
      box-shadow: 0 0 0 2px rgba(255, 196, 0, 0.55);
      transition: opacity 0.5s ease-out 0.7s;
    }
    .plars-jump-flash.fade { opacity: 0; }`;
  document.head.appendChild(style);

  // the text element containing (x, y), else the nearest one on that line
  function target(x, y) {
    let best = null, bestScore = Infinity;
    for (const el of document.querySelectorAll('.typst-text')) {
      const r = el.getBoundingClientRect();
      if (!r.width || !r.height) continue;
      const dx = Math.max(r.left - x, 0, x - r.right);
      const dy = Math.max(r.top - y, 0, y - r.bottom);
      const score = dy * 4 + dx + (dx === 0 && dy === 0 ? -1e6 + r.width * r.height * 1e-3 : 0);
      if (score < bestScore) { best = r; bestScore = score; }
    }
    return bestScore < 200 ? best : null;
  }

  function flash(ripple) {
    // the ripple lives in the preview's scroll container; anchor the flash
    // there too so it moves with tinymist's smooth scroll
    const parent = ripple.parentElement;
    if (parent && getComputedStyle(parent).position === 'static') parent.style.position = 'relative';
    const p = ripple.getBoundingClientRect();
    const r = target(p.left, p.top);
    if (!r || !parent) return;
    const base = parent.getBoundingClientRect();
    const pad = 2;
    const box = document.createElement('div');
    box.className = 'plars-jump-flash';
    box.style.left = (r.left - base.left + parent.scrollLeft - pad) + 'px';
    box.style.top = (r.top - base.top + parent.scrollTop - pad) + 'px';
    box.style.width = (r.width + 2 * pad) + 'px';
    box.style.height = (r.height + 2 * pad) + 'px';
    document.querySelectorAll('.plars-jump-flash').forEach(e => e.remove());
    parent.appendChild(box);
    requestAnimationFrame(() => requestAnimationFrame(() => box.classList.add('fade')));
    box.addEventListener('transitionend', () => box.remove());
  }

  new MutationObserver(records => {
    for (const rec of records)
      for (const n of rec.addedNodes)
        if (n.classList && n.classList.contains('typst-jump-ripple')) flash(n);
  }).observe(document.documentElement, { childList: true, subtree: true });
})();
"#;
