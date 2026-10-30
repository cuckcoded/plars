//! The page view of the built-in bitmap preview: a scrollable column of page
//! images rendered by [`crate::render`].
//!
//! Pages are laid out at their final size as soon as a compile finishes, but
//! only the ones on or near the screen are rasterized, at the current zoom
//! times the display scale. A changed page keeps showing its old image until
//! the new one arrives, so typing doesn't make the preview flicker.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, glib};

use crate::render::{PageInfo, Renderer, Request, Response, Spot};

const MARGIN: f64 = 16.0;
const MIN_ZOOM: f64 = 0.1;
const MAX_ZOOM: f64 = 8.0;

thread_local!(static CSS: gtk::CssProvider = {
    let p = gtk::CssProvider::new();
    p.load_from_string(
        ".plars-pages { background-color: alpha(@window_fg_color, 0.08); }\
         .plars-page { background-color: white; box-shadow: 0 1px 4px alpha(black, 0.3); }\
         .plars-pages.inverted .plars-page { filter: invert(1) hue-rotate(180deg); }\
         .plars-flash { border-radius: 3px; background-color: alpha(#ffc400, 0.38);\
                        box-shadow: 0 0 0 2px alpha(#ffc400, 0.55); transition: opacity 500ms ease-out 700ms; }\
         .plars-flash.fade { opacity: 0; }");
    gtk::style_context_add_provider_for_display(&gdk::Display::default().unwrap(), &p,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    p
});

struct Slot {
    frame: gtk::Overlay,
    picture: gtk::Picture,
    info: PageInfo,
    /// (page hash, pixel_per_pt) of the image on screen
    shown: Option<(u128, f64)>,
}

type JumpFn = Rc<dyn Fn(PathBuf, usize, usize)>;

pub struct PageView {
    pub root: gtk::Overlay,
    /// Zoom buttons for the preview's bar.
    pub controls: gtk::Box,
    scroller: gtk::ScrolledWindow,
    column: gtk::Box,
    error: gtk::Revealer,
    error_label: gtk::Label,
    zoom_label: gtk::Button,
    slots: RefCell<Vec<Slot>>,
    /// None fits the page width to the pane.
    zoom: Cell<Option<f64>>,
    /// Logical pixels per pt currently laid out.
    scale: Cell<f64>,
    renderer: RefCell<Option<Renderer>>,
    pending: RefCell<HashSet<(usize, u64)>>,
    update_queued: Cell<bool>,
    last_compile: RefCell<Option<(PathBuf, PathBuf, HashMap<PathBuf, String>)>>,
    /// A spot to scroll to once the pages are laid out.
    reveal: RefCell<Option<Spot>>,
    on_jump: RefCell<Option<JumpFn>>,
}

impl PageView {
    pub fn new() -> Rc<PageView> {
        CSS.with(|_| {});
        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(MARGIN as i32)
            .halign(gtk::Align::Center)
            .margin_top(MARGIN as i32).margin_bottom(MARGIN as i32)
            .margin_start(MARGIN as i32).margin_end(MARGIN as i32)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&column)
            .vexpand(true)
            .hexpand(true)
            .css_classes(["plars-pages"])
            .build();
        let error_label = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .margin_start(10).margin_end(10).margin_top(6).margin_bottom(6)
            .build();
        let error_bar = gtk::Box::builder().css_classes(["osd", "toolbar"]).build();
        error_bar.append(&error_label);
        let error = gtk::Revealer::builder()
            .child(&error_bar)
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .valign(gtk::Align::End)
            .build();
        let root = gtk::Overlay::builder().child(&scroller).build();
        root.add_overlay(&error);

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        controls.add_css_class("linked");
        let zoom_out = gtk::Button::builder().icon_name("zoom-out-symbolic").tooltip_text("Zoom out").css_classes(["flat"]).build();
        let zoom_label = gtk::Button::builder().label("Fit").tooltip_text("Fit page width").css_classes(["flat", "caption"]).build();
        let zoom_in = gtk::Button::builder().icon_name("zoom-in-symbolic").tooltip_text("Zoom in").css_classes(["flat"]).build();
        controls.append(&zoom_out);
        controls.append(&zoom_label);
        controls.append(&zoom_in);

        let view = Rc::new(PageView {
            root,
            controls,
            scroller,
            column,
            error,
            error_label,
            zoom_label: zoom_label.clone(),
            slots: RefCell::default(),
            zoom: Cell::new(None),
            scale: Cell::new(1.0),
            renderer: RefCell::default(),
            pending: RefCell::default(),
            update_queued: Cell::new(false),
            last_compile: RefCell::default(),
            reveal: RefCell::default(),
            on_jump: RefCell::default(),
        });

        let w = Rc::downgrade(&view);
        zoom_out.connect_clicked(move |_| if let Some(v) = w.upgrade() { v.zoom_by(1.0 / 1.2) });
        let w = Rc::downgrade(&view);
        zoom_in.connect_clicked(move |_| if let Some(v) = w.upgrade() { v.zoom_by(1.2) });
        let w = Rc::downgrade(&view);
        zoom_label.connect_clicked(move |_| if let Some(v) = w.upgrade() { v.set_zoom(None) });

        // Ctrl+scroll zooms
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let w = Rc::downgrade(&view);
        scroll.connect_scroll(move |c, _, dy| {
            let Some(v) = w.upgrade() else { return glib::Propagation::Proceed };
            if !c.current_event_state().contains(gdk::ModifierType::CONTROL_MASK) {
                return glib::Propagation::Proceed;
            }
            v.zoom_by(1.1f64.powf(-dy));
            glib::Propagation::Stop
        });
        view.scroller.add_controller(scroll);

        // the width changes with the pane; the visible range with scrolling
        let w = Rc::downgrade(&view);
        view.scroller.hadjustment().connect_changed(move |_| if let Some(v) = w.upgrade() { v.relayout() });
        let w = Rc::downgrade(&view);
        view.scroller.vadjustment().connect_value_changed(move |_| if let Some(v) = w.upgrade() { v.queue_update() });
        let w = Rc::downgrade(&view);
        view.scroller.vadjustment().connect_changed(move |_| if let Some(v) = w.upgrade() { v.queue_update() });
        let w = Rc::downgrade(&view);
        view.root.connect_scale_factor_notify(move |_| if let Some(v) = w.upgrade() { v.queue_update() });
        view
    }

    /// Click in a page -> `f(file, line, column in chars)`.
    pub fn connect_jump(&self, f: impl Fn(PathBuf, usize, usize) + 'static) {
        *self.on_jump.borrow_mut() = Some(Rc::new(f));
    }

    pub fn set_inverted(&self, inverted: bool) {
        if inverted {
            self.scroller.add_css_class("inverted");
        } else {
            self.scroller.remove_css_class("inverted");
        }
    }

    /// Compile `main` with the given unsaved buffers; the view updates when
    /// the worker is done.
    pub fn compile(self: &Rc<Self>, root: &Path, main: &Path, overlay: HashMap<PathBuf, String>) {
        *self.last_compile.borrow_mut() = Some((root.to_owned(), main.to_owned(), overlay.clone()));
        self.renderer().send(Request::Compile { root: root.to_owned(), main: main.to_owned(), overlay });
    }

    /// Compile again, e.g. after an image changed on disk.
    pub fn recompile(self: &Rc<Self>) {
        let last = self.last_compile.borrow().clone();
        if let Some((root, main, overlay)) = last {
            self.compile(&root, &main, overlay);
        }
    }

    /// Show where the caret at (line, column in chars) of `path` ends up.
    pub fn sync_cursor(self: &Rc<Self>, path: &Path, line: usize, column: usize) {
        if !self.slots.borrow().is_empty() {
            self.renderer().send(Request::Cursor { path: path.to_owned(), line, column });
        }
    }

    /// Forget the current document, e.g. when previewing another file.
    pub fn clear(&self) {
        for s in self.slots.borrow_mut().drain(..) {
            self.column.remove(&s.frame);
        }
        self.pending.borrow_mut().clear();
        self.last_compile.borrow_mut().take();
        self.error.set_reveal_child(false);
        self.scroller.vadjustment().set_value(0.0);
    }

    fn renderer(self: &Rc<Self>) -> std::cell::Ref<'_, Renderer> {
        if self.renderer.borrow().is_none() {
            // the worker answers from its thread; hop over to the main loop
            let (tx, rx) = async_channel::unbounded();
            let renderer = Renderer::spawn(move |r| {
                let _ = tx.send_blocking(r);
            });
            let w = Rc::downgrade(self);
            glib::spawn_future_local(async move {
                while let Ok(r) = rx.recv().await {
                    match w.upgrade() {
                        Some(v) => v.on_response(r),
                        None => break,
                    }
                }
            });
            *self.renderer.borrow_mut() = Some(renderer);
        }
        std::cell::Ref::map(self.renderer.borrow(), |r| r.as_ref().unwrap())
    }

    fn on_response(self: &Rc<Self>, r: Response) {
        match r {
            Response::Compiled { pages, .. } => {
                self.error.set_reveal_child(false);
                self.set_pages(pages);
            }
            Response::Failed { errors } => {
                let text = match errors.len() {
                    0 => "Compile error".to_owned(),
                    1 => errors[0].clone(),
                    n => format!("{}\n(+{} more)", errors[0], n - 1),
                };
                self.error_label.set_label(&text);
                self.error.set_reveal_child(true);
            }
            Response::Rendered { page, hash, pixel_per_pt, width, height, rgba } => {
                self.pending.borrow_mut().remove(&(page, pixel_per_pt.to_bits()));
                let mut slots = self.slots.borrow_mut();
                let Some(slot) = slots.get_mut(page) else { return };
                if slot.info.hash != hash || pixel_per_pt != self.pixel_per_pt() {
                    return; // outdated
                }
                let texture = gdk::MemoryTexture::new(width as i32, height as i32,
                    gdk::MemoryFormat::R8g8b8a8Premultiplied, &glib::Bytes::from_owned(rgba), width as usize * 4);
                slot.picture.set_paintable(Some(&texture));
                slot.shown = Some((hash, pixel_per_pt));
            }
            Response::JumpToSource { path, line, column } => {
                let f = self.on_jump.borrow().clone();
                if let Some(f) = f {
                    f(path, line, column);
                }
            }
            Response::JumpToUrl(url) => {
                let root = self.root.root().and_downcast::<gtk::Window>();
                gtk::UriLauncher::new(&url).launch(root.as_ref(), gtk::gio::Cancellable::NONE, |_| {});
            }
            Response::JumpToSpot(spot) => self.reveal(spot, true),
            Response::CursorSpots(spots) => {
                if let Some(spot) = spots.first() {
                    self.reveal(*spot, true);
                }
            }
        }
    }

    fn set_pages(self: &Rc<Self>, pages: Vec<PageInfo>) {
        {
            let mut slots = self.slots.borrow_mut();
            while slots.len() > pages.len() {
                let s = slots.pop().unwrap();
                self.column.remove(&s.frame);
            }
            for (i, info) in pages.into_iter().enumerate() {
                match slots.get_mut(i) {
                    Some(s) => s.info = info,
                    None => slots.push(self.new_slot(i, info)),
                }
            }
        }
        self.pending.borrow_mut().clear();
        self.relayout();
        self.queue_update();
    }

    fn new_slot(self: &Rc<Self>, index: usize, info: PageInfo) -> Slot {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Fill)
            .can_shrink(true)
            .css_classes(["plars-page"])
            .build();
        let frame = gtk::Overlay::builder().child(&picture).halign(gtk::Align::Center).build();
        let click = gtk::GestureClick::new();
        let w = Rc::downgrade(self);
        click.connect_released(move |_, n, x, y| {
            let Some(v) = w.upgrade() else { return };
            if n == 1 {
                let s = v.scale.get();
                v.renderer().send(Request::Click { page: index, x: x / s, y: y / s });
            }
        });
        frame.add_controller(click);
        frame.set_cursor_from_name(Some("pointer"));
        self.column.append(&frame);
        Slot { frame, picture, info, shown: None }
    }

    fn set_zoom(self: &Rc<Self>, zoom: Option<f64>) {
        // keep the middle of the view in place
        let adj = self.scroller.vadjustment();
        let anchor = (adj.value() + adj.page_size() / 2.0) / self.scale.get();
        self.zoom.set(zoom.map(|z| z.clamp(MIN_ZOOM, MAX_ZOOM)));
        self.relayout();
        let s = self.scale.get();
        let w = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(v) = w.upgrade() {
                let adj = v.scroller.vadjustment();
                adj.set_value(anchor * s - adj.page_size() / 2.0);
            }
        });
    }

    fn zoom_by(self: &Rc<Self>, factor: f64) {
        self.set_zoom(Some(self.scale.get() * factor));
    }

    /// Size the pages for the current zoom.
    fn relayout(self: &Rc<Self>) {
        let slots = self.slots.borrow();
        let scale = match self.zoom.get() {
            Some(z) => z,
            None => {
                let avail = self.scroller.hadjustment().page_size() - 2.0 * MARGIN;
                let widest = slots.iter().map(|s| s.info.width).fold(0.0, f64::max);
                if avail <= 0.0 || widest <= 0.0 {
                    return;
                }
                (avail / widest).clamp(MIN_ZOOM, MAX_ZOOM)
            }
        };
        self.scale.set(scale);
        self.zoom_label.set_label(&match self.zoom.get() {
            Some(z) => format!("{:.0}%", z * 100.0),
            None => "Fit".into(),
        });
        for s in slots.iter() {
            let (w, h) = ((s.info.width * scale).round() as i32, (s.info.height * scale).round() as i32);
            if s.frame.size_request() != (w, h) {
                s.frame.set_size_request(w, h);
            }
        }
        drop(slots);
        self.queue_update();
    }

    fn pixel_per_pt(&self) -> f64 {
        self.scale.get() * self.root.scale_factor() as f64
    }

    /// Run [`Self::update`] once the pending layout has happened.
    fn queue_update(self: &Rc<Self>) {
        if self.update_queued.replace(true) {
            return;
        }
        let w = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            if let Some(v) = w.upgrade() {
                v.update_queued.set(false);
                v.update();
            }
        });
    }

    /// Request images for the pages near the screen; drop far-away ones.
    fn update(self: &Rc<Self>) {
        if let Some(spot) = self.reveal.borrow_mut().take() {
            self.scroll_to(spot);
        }
        let height = self.scroller.height() as f32;
        if height <= 0.0 {
            return;
        }
        let ppt = self.pixel_per_pt();
        let mut wanted = Vec::new();
        for (i, s) in self.slots.borrow_mut().iter_mut().enumerate() {
            let Some(b) = s.frame.compute_bounds(&self.scroller) else { continue };
            let (top, bottom) = (b.y(), b.y() + b.height());
            if bottom > -height && top < 2.0 * height {
                if s.shown != Some((s.info.hash, ppt)) {
                    wanted.push(i);
                }
            } else if (bottom < -3.0 * height || top > 4.0 * height) && s.shown.is_some() {
                s.picture.set_paintable(gdk::Paintable::NONE);
                s.shown = None;
            }
        }
        if wanted.is_empty() {
            return;
        }
        let renderer = self.renderer();
        let mut pending = self.pending.borrow_mut();
        for page in wanted {
            if pending.insert((page, ppt.to_bits())) {
                renderer.send(Request::Render { page, pixel_per_pt: ppt });
            }
        }
    }

    /// Scroll `spot` into view (if it isn't already) and maybe flash it.
    fn reveal(self: &Rc<Self>, spot: Spot, flash: bool) {
        if flash {
            self.flash(spot);
        }
        *self.reveal.borrow_mut() = Some(spot);
        self.queue_update();
    }

    fn scroll_to(&self, spot: Spot) {
        let slots = self.slots.borrow();
        let Some(slot) = slots.get(spot.page) else { return };
        let Some(b) = slot.frame.compute_bounds(&self.scroller) else { return };
        let s = self.scale.get();
        let adj = self.scroller.vadjustment();
        let (top, bottom) = (b.y() as f64 + spot.y * s, b.y() as f64 + (spot.y + spot.h) * s);
        if top < 0.0 || bottom > adj.page_size() {
            adj.set_value(adj.value() + top - adj.page_size() / 3.0);
        }
    }

    fn flash(&self, spot: Spot) {
        let slots = self.slots.borrow();
        let Some(slot) = slots.get(spot.page) else { return };
        let (s, pad) = (self.scale.get(), 2.0);
        let mark = gtk::Box::builder()
            .css_classes(["plars-flash"])
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_start((spot.x * s - pad).max(0.0) as i32)
            .margin_top((spot.y * s - pad).max(0.0) as i32)
            .width_request((spot.w * s + 2.0 * pad) as i32)
            .height_request((spot.h * s + 2.0 * pad) as i32)
            .can_target(false)
            .build();
        slot.frame.add_overlay(&mark);
        let m = mark.clone();
        glib::timeout_add_local_once(Duration::from_millis(30), move || m.add_css_class("fade"));
        let frame = slot.frame.clone();
        glib::timeout_add_local_once(Duration::from_millis(1400), move || frame.remove_overlay(&mark));
    }
}
