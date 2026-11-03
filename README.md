# Plars

A GTK4 / libadwaita Typst editor written in Rust: a clean-room Linux clone of
the macOS app [Edist](https://edist.app). Language intelligence, the live
preview, export and the debugger all come from
[tinymist](https://github.com/Myriad-Dreamin/tinymist). Alternatively, a
built-in renderer (*Preferences → Typst → Built-in renderer*) compiles the
document in-process with the `typst` crates and shows the pages as bitmaps.

## Build & run

Requirements (Arch package names): `gtk4 libadwaita gtksourceview5 webkitgtk-6.0`,
a Rust toolchain, and `tinymist` (`sudo pacman -S tinymist`). The `typst` CLI is
optional; it's used to create projects from templates.

```sh
cargo run --release -- path/to/project     # or a .typ file
make install                               # to ~/.local (PREFIX=... to change)
```

### macOS and Windows

WebKitGTK isn't packaged for macOS or Windows, so build there without the
`webkit` feature. The tinymist preview and the docs then open in your web
browser (the preview still follows the caret); the built-in renderer works
inside the window as on Linux:

```sh
brew install gtk4 libadwaita gtksourceview5 adwaita-icon-theme   # macOS
pacman -S mingw-w64-ucrt-x86_64-{rust,gtk4,libadwaita,gtksourceview5}   # Windows, MSYS2 UCRT64
cargo build --release --no-default-features
```

Homebrew's GTK doesn't pull in an icon theme, so without `adwaita-icon-theme`
most buttons show a "missing image" icon. The macOS release ships the theme
inside `Plars.app` (`Contents/Resources/share/icons`), which Plars adds to
GTK's icon search path at startup, so it doesn't need that formula. The
bundle is put together by the CI's *Package* step from
`data/macos/Info.plist` and `data/icons/io.github.plars.Plars.svg`. When
started from Finder, Plars also looks for tinymist and typst in
`/opt/homebrew/bin`, `/usr/local/bin`, `~/.cargo/bin` and `~/.local/bin`.

CI (`.github/workflows/build.yml`) builds all three platforms on every push.
Pushing a `v*` tag publishes a GitHub release with a Linux tarball, a macOS
`Plars.app` zip (needs the Homebrew GTK libraries) and a self-contained
Windows zip.

If tinymist isn't on `$PATH`, set its location in *Preferences → Typst*, or
run with `PLARS_TINYMIST=/path/to/tinymist`. `PLARS_DEBUG=1` mirrors the
tinymist log panel to stderr.

## Features

| Edist (macOS) | This clone | How |
|---|---|---|
| Project folder sidebar, tabs | ✓ | `GtkDirectoryList` tree, `AdwTabView` |
| Syntax highlighting | ✓ | `data/language-specs/typst.lang` (markup / code / math modes) |
| Live preview, follows caret (flashing the matching element on line changes), lock to file, detach, partial rendering | ✓ | `tinymist.doStartPreview` (`--partial-rendering`) in WebKitGTK, throttled `tinymist.scrollPreview` (`panelScrollTo`; `changeCursorPosition` is disabled in tinymist 0.15), `tinymist.pinMain` |
| Click in preview → jump to source | ✓ | `window/showDocument` / `tinymist/preview/scrollSource` |
| Built-in bitmap preview (extra, switch in Preferences) | ✓ | `typst::compile` on a worker thread with the unsaved buffers overlaid; only pages near the screen are rasterized with `typst-render`, at zoom × display scale; `typst-ide` jumps for click → source and caret → page |
| Diagnostics + Problems list | ✓ | `textDocument/publishDiagnostics` |
| Completion, hover docs, go to definition, format | ✓ | LSP (`completion` with snippets, `hover`, `definition`, `formatting` via typstyle) |
| Outline ("On This Page") | ✓ | `textDocument/documentSymbol` |
| Export PDF / PNG / SVG (+ HTML, Markdown) | ✓ | `tinymist.export*` |
| Find / replace in file and in project | ✓ | `GtkSourceSearchContext`, regex over project files |
| New project from Typst Universe template | ✓ | `typst init` (fallback `tinymist.doInitTemplate`) |
| Typst documentation window | ✓ | WebKit on typst.app/docs |
| Keymaps: Plars, VS Code, JetBrains, Sublime Text, Emacs, Vim | ✓ | `keymap.rs` tables with multi-key sequences (`Ctrl+K Ctrl+C`, `C-x C-s`); Vim via `GtkSourceVimIMContext`. On macOS the tables' Ctrl becomes Command (GTK reports it as Meta), except in the Emacs keymap |
| Multiple carets, block selection | ✓ | add next match / all matches, caret column above/below (VS Code: Ctrl+Shift+↑/↓), Alt+Click, middle-button or Shift+Alt drag for a block selection, split selection; typing, completion, paste and line commands apply at every caret |
| Themes, font settings | ✓ | style schemes |
| **Debugger** (Linux extra) | ✓ | `tinymist dap`: breakpoints, call stack, variables, REPL evaluate |
| Typing sounds, handwriting symbol recogniser, licensing | ✗ | not cloned |

## Architecture

```
src/
  main.rs       app setup, actions
  keymap.rs     keymaps (key sequences → commands) and the accelerators they install
  window.rs     main window; glue between editor, tinymist, preview, panels
  editor.rs     one tab: GtkSourceView + LSP completion/hover providers, marks, search
  editor/carets.rs    PlarsView (draws extra carets), caret motion, edit replication
  editor/commands.rs  editor commands bound by keymaps (lines, comments, selection, …)
  rpc.rs        Content-Length framed JSON over child stdio (shared by LSP and DAP)
  lsp.rs        tinymist language-server client
  dap.rs        tinymist debug-adapter client
  debugger.rs   debug panel (stack, lazy variable tree, console)
  preview.rs    preview pane: tinymist's preview in WebKit, or the page view
  pageview.rs   built-in preview: page images, zoom, visible-page rendering
  render.rs     built-in preview: Typst World, compile/render worker thread
  sidebar.rs    file tree, outline, project search
  panels.rs     problems + server log
  dialogs.rs    preferences, templates, docs window
  settings.rs   JSON settings in ~/.config/plars/settings.json
```

### Keymaps and multiple carets

Pick a keymap under Preferences → Editor → Keymap; main menu → Keyboard Shortcuts (Ctrl+?)
lists the active bindings. Each keymap is a table of key sequences mapped to
an editor command (`line.move-up`, `cursor.add-next-match`, …) or a window
action (`win.save`). A capture-phase key controller on the editor resolves
sequences. Single-key window bindings are also installed as application
accelerators, and accelerators are suspended while a prefix such as `C-x` is
pending, so the second key reaches the editor.

GtkSourceView has a single cursor. Extra carets are pairs of text marks stored
on a `GtkSourceView` subclass, which draws them in `snapshot_layer`. Edits
GTK makes at the primary caret (typing, Enter with auto-indent, paste) are
recorded from `insert-text`/`delete-range` and replayed at the other carets
when the user action ends. Motion, deletion, completion and line commands
handle every caret directly.

Reader threads parse frames and hand them to the GTK main loop through an
`async_channel`. All UI state lives on the main thread in `Rc<RefCell<…>>`.

## tinymist quirks found while building this

* **DAP entry pause.** `tinymist dap` always pauses at the start of the
  document, but it only *reports* that pause when `stopOnEntry: true`. The
  client therefore always asks for it and continues automatically unless
  "Stop on entry" is ticked.
* **DAP breakpoint timing.** Breakpoints sent before that entry pause are
  silently dropped. `setBreakpoints` and `configurationDone` are sent when the
  `stopped(reason: entry)` event arrives.
* **DAP arguments.** `configurationDone` and `threads` reject `"arguments": {}`.
  The field must be omitted.
* **No stepping yet.** tinymist 0.15's adapter answers `next`, `stepIn`,
  `stepOut` and `pause` with "method not found". The buttons disable
  themselves after the first refusal. Breakpoints and Continue work.
  Breakpoints bind to the nearest function/block hook, so they pause on entry
  to the function containing the line.
* **No compile-status notifications.** `tinymist/compileStatus` isn't sent to
  generic clients, so the status bar is driven by diagnostics.

## How the original was studied

The `.dmg` was unpacked with `7z`. `Info.plist`, the resource folder and the
binary's UI strings (`strings`) gave the feature list: tabs, project sidebar,
"Lock Preview to This File", "Follow Cursor in Preview", "Open preview in a
separate window", "Export PDF in Place", "Find in Project", templates from
Typst Universe, Vim key bindings, the docs window, and so on. No code or
assets were copied; this is an independent implementation on GTK and tinymist.
