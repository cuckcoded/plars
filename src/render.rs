//! The built-in bitmap preview backend: the Typst compiler runs in-process on
//! a worker thread and `typst-render` rasterizes pages on request.
//!
//! Unlike tinymist's preview this needs neither tinymist nor a web view. The
//! editor hands over the text of its open buffers with every compile, so
//! unsaved edits show up just as they do in tinymist's preview.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use typst::diag::{FileResult, Severity, SourceDiagnostic};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::introspection::PagedPosition;
use typst::layout::{Abs, Frame, FrameItem, Point, Transform};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::{hash128, LazyHash, Scalar};
use typst::{Library, LibraryExt, World, WorldExt};
use typst_ide::{IdeWorld, Jump};
use typst_kit::datetime::Time;
use typst_kit::downloader::SystemDownloader;
use typst_kit::files::{FileLoader, FileStore, FsRoot, SystemFiles};
use typst_kit::fonts::FontStore;
use typst_kit::packages::SystemPackages;
use typst_layout::PagedDocument;

pub enum Request {
    /// Compile `main` within `root`; `overlay` maps canonical paths to the
    /// text of open editor buffers, which win over the files on disk.
    Compile { root: PathBuf, main: PathBuf, overlay: HashMap<PathBuf, String> },
    /// Rasterize one page of the last document at `pixel_per_pt`.
    Render { page: usize, pixel_per_pt: f64 },
    /// A click at `point` (in pt) on `page` (0-based).
    Click { page: usize, x: f64, y: f64 },
    /// Where does the caret at (line, column in chars) of `path` show up?
    Cursor { path: PathBuf, line: usize, column: usize },
}

#[derive(Clone, Copy, PartialEq)]
pub struct PageInfo {
    pub width: f64,
    pub height: f64,
    /// Changes whenever the page looks different.
    pub hash: u128,
}

/// A rectangle on a page, in pt.
#[derive(Clone, Copy, Debug)]
pub struct Spot {
    pub page: usize,
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

pub enum Response {
    Compiled { pages: Vec<PageInfo> },
    /// The document failed to compile; the last good one stays.
    Failed { errors: Vec<String> },
    Rendered { page: usize, hash: u128, pixel_per_pt: f64, width: u32, height: u32, rgba: Vec<u8> },
    JumpToSource { path: PathBuf, line: usize, column: usize },
    JumpToUrl(String),
    JumpToSpot(Spot),
    CursorSpots(Vec<Spot>),
}

/// Handle to the worker thread; dropping it stops the thread.
pub struct Renderer {
    tx: mpsc::Sender<Request>,
}

impl Renderer {
    pub fn spawn(on_response: impl Fn(Response) + Send + 'static) -> Renderer {
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("plars-render".into())
            .spawn(move || Worker::new().run(rx, on_response))
            .expect("spawn render thread");
        Renderer { tx }
    }

    pub fn send(&self, req: Request) {
        let _ = self.tx.send(req);
    }
}

struct Worker {
    world: Option<PlarsWorld>,
    fonts: Arc<FontStore>,
    library: Arc<LazyHash<Library>>,
    doc: Option<PagedDocument>,
}

impl Worker {
    fn new() -> Worker {
        let mut fonts = FontStore::new();
        fonts.extend(typst_kit::fonts::system());
        fonts.extend(typst_kit::fonts::embedded());
        Worker {
            world: None,
            fonts: Arc::new(fonts),
            library: Arc::new(LazyHash::new(Library::builder().build())),
            doc: None,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Request>, out: impl Fn(Response)) {
        while let Ok(first) = rx.recv() {
            // Work through everything queued up meanwhile, skipping requests
            // that a later one makes moot: only the newest compile matters,
            // and renders of a page at an outdated zoom are wasted effort.
            let mut batch = vec![first];
            batch.extend(rx.try_iter());
            if let Some(i) = batch.iter().rposition(|r| matches!(r, Request::Compile { .. })) {
                let Request::Compile { root, main, overlay } = batch.remove(i) else { unreachable!() };
                batch.retain(|r| !matches!(r, Request::Compile { .. }));
                out(self.compile(root, &main, overlay));
            }
            let mut renders: Vec<(usize, f64)> = Vec::new();
            let mut rest = Vec::new();
            for r in batch {
                match r {
                    Request::Render { page, pixel_per_pt } => {
                        renders.retain(|(p, _)| *p != page);
                        renders.push((page, pixel_per_pt));
                    }
                    other => rest.push(other),
                }
            }
            for (page, ppt) in renders {
                if let Some(r) = self.render(page, ppt) {
                    out(r);
                }
            }
            for r in rest {
                let resp = match r {
                    Request::Click { page, x, y } => self.click(page, x, y),
                    Request::Cursor { path, line, column } => self.cursor(&path, line, column),
                    _ => None,
                };
                if let Some(r) = resp {
                    out(r);
                }
            }
        }
    }

    fn compile(&mut self, root: PathBuf, main: &Path, overlay: HashMap<PathBuf, String>) -> Response {
        let Some(main_id) = file_id(&root, main) else {
            return Response::Failed { errors: vec![format!("{} is outside the project folder", main.display())] };
        };
        match &mut self.world {
            Some(w) if w.root == root => {
                w.main = main_id;
                w.files.loader_mut().overlay = overlay;
                w.files.reset();
                w.time.reset();
            }
            _ => {
                self.world = Some(PlarsWorld::new(root, main_id, overlay, self.fonts.clone(), self.library.clone()));
                self.doc = None;
            }
        }
        let world = self.world.as_ref().unwrap();
        let result = typst::compile::<PagedDocument>(world);
        match result.output {
            Ok(doc) => {
                let pages = doc.pages().iter()
                    .map(|p| PageInfo {
                        width: p.frame.width().to_pt(),
                        height: p.frame.height().to_pt(),
                        hash: hash128(p),
                    })
                    .collect();
                self.doc = Some(doc);
                Response::Compiled { pages }
            }
            Err(errors) => Response::Failed {
                errors: errors.iter().filter(|d| d.severity == Severity::Error).map(|d| world.describe(d)).collect(),
            },
        }
    }

    fn render(&self, page: usize, pixel_per_pt: f64) -> Option<Response> {
        let p = self.doc.as_ref()?.pages().get(page)?;
        let pixmap = typst_render::render(p, &typst_render::RenderOptions {
            pixel_per_pt: Scalar::new(pixel_per_pt),
            render_bleed: false,
        });
        let (width, height) = (pixmap.width(), pixmap.height());
        Some(Response::Rendered { page, hash: hash128(p), pixel_per_pt, width, height, rgba: pixmap.take() })
    }

    fn click(&self, page: usize, x: f64, y: f64) -> Option<Response> {
        let (world, doc) = (self.world.as_ref()?, self.doc.as_ref()?);
        let pos = PagedPosition { page: NonZeroUsize::new(page + 1)?, point: Point::new(Abs::pt(x), Abs::pt(y)) };
        Some(match typst_ide::jump_from_click(world, doc, &pos)? {
            Jump::File(id, offset) => {
                let path = world.files.loader().system.resolve(id).ok()?;
                let (line, column) = world.source(id).ok()?.lines().byte_to_line_column(offset)?;
                Response::JumpToSource { path, line, column }
            }
            Jump::Url(url) => Response::JumpToUrl(url.to_string()),
            Jump::Position(p) => Response::JumpToSpot(Spot {
                page: p.page.get() - 1, x: p.point.x.to_pt(), y: p.point.y.to_pt(), w: 0.0, h: 0.0,
            }),
        })
    }

    fn cursor(&self, path: &Path, line: usize, column: usize) -> Option<Response> {
        let (world, doc) = (self.world.as_ref()?, self.doc.as_ref()?);
        let source = world.source(file_id(&world.root, path)?).ok()?;
        let offset = source.lines().line_column_to_byte(line, column)?;
        let spots = typst_ide::jump_from_cursor(doc, &source, offset).into_iter()
            .map(|pos| {
                let page = pos.page.get() - 1;
                text_box(&doc.pages()[page].frame, pos.point).map_or(
                    Spot { page, x: pos.point.x.to_pt(), y: pos.point.y.to_pt() - 10.0, w: 2.0, h: 12.0 },
                    |(x, y, w, h)| Spot { page, x, y, w, h },
                )
            })
            .collect();
        Some(Response::CursorSpots(spots))
    }
}

/// The bounds of the text run that `point` (as returned by
/// `jump_from_cursor`, i.e. on a glyph's baseline) belongs to.
fn text_box(frame: &Frame, point: Point) -> Option<(f64, f64, f64, f64)> {
    fn walk(frame: &Frame, ts: Transform, point: Point) -> Option<(f64, f64, f64, f64)> {
        for (pos, item) in frame.items() {
            match item {
                FrameItem::Group(g) => {
                    let ts = ts.pre_concat(Transform::translate(pos.x, pos.y)).pre_concat(g.transform);
                    if let Some(r) = walk(&g.frame, ts, point) {
                        return Some(r);
                    }
                }
                FrameItem::Text(t) => {
                    let origin = pos.transform(ts);
                    let (w, size) = (t.width().to_pt(), t.size.to_pt());
                    let (x, y) = (origin.x.to_pt(), origin.y.to_pt());
                    let (px, py) = (point.x.to_pt(), point.y.to_pt());
                    if (py - y).abs() < 0.5 && px >= x - 0.5 && px <= x + w + 0.5 {
                        return Some((x, y - size * 0.8, w, size));
                    }
                }
                _ => {}
            }
        }
        None
    }
    walk(frame, Transform::identity(), point)
}

fn file_id(root: &Path, path: &Path) -> Option<FileId> {
    let vpath = VirtualPath::virtualize(root, path).ok()?;
    Some(RootedPath::new(VirtualRoot::Project, vpath).intern())
}

/// Files come from disk and packages from the usual Typst package
/// directories, except that open editor buffers override their files.
struct Loader {
    system: SystemFiles,
    overlay: HashMap<PathBuf, String>,
}

impl FileLoader for Loader {
    fn load(&self, id: FileId) -> FileResult<Bytes> {
        if id.root() == &VirtualRoot::Project {
            if let Some(text) = self.system.resolve(id).ok().and_then(|p| self.overlay.get(&p)) {
                return Ok(Bytes::from_string(text.clone()));
            }
        }
        self.system.load(id)
    }
}

struct PlarsWorld {
    root: PathBuf,
    main: FileId,
    library: Arc<LazyHash<Library>>,
    fonts: Arc<FontStore>,
    files: FileStore<Loader>,
    time: Time,
}

impl PlarsWorld {
    fn new(root: PathBuf, main: FileId, overlay: HashMap<PathBuf, String>, fonts: Arc<FontStore>,
           library: Arc<LazyHash<Library>>) -> PlarsWorld {
        let packages = SystemPackages::new(SystemDownloader::new(concat!("plars/", env!("CARGO_PKG_VERSION"))));
        let system = SystemFiles::new(FsRoot::new(root.clone()), packages);
        PlarsWorld {
            root,
            main,
            library,
            fonts,
            files: FileStore::new(Loader { system, overlay }),
            time: Time::system(),
        }
    }

    /// "file.typ:3: message" for the preview's error bar.
    fn describe(&self, d: &SourceDiagnostic) -> String {
        let place = d.span.id().and_then(|id| {
            let line = self.range(d.span).and_then(|r| self.source(id).ok()?.lines().byte_to_line(r.start))?;
            let path = self.files.loader().system.resolve(id).ok()?;
            let name = path.strip_prefix(&self.root).unwrap_or(&path).display().to_string();
            Some(format!("{name}:{}: ", line + 1))
        });
        format!("{}{}", place.unwrap_or_default(), d.message)
    }
}

impl World for PlarsWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        self.fonts.book()
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        self.files.source(id)
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        self.files.file(id)
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.font(index)
    }

    fn today(&self, offset: Option<Duration>) -> Option<Datetime> {
        self.time.today(offset)
    }
}

impl IdeWorld for PlarsWorld {
    fn upcast(&self) -> &dyn World {
        self
    }
}
