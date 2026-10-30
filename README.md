# Plars

A GTK4 / libadwaita Typst editor written in Rust: a clean-room Linux clone of
the macOS app [Edist](https://edist.app). Language intelligence, the live
preview, export and the debugger all come from
[tinymist](https://github.com/Myriad-Dreamin/tinymist).

## Build & run

Requirements (Arch package names): `gtk4 libadwaita gtksourceview5 webkitgtk-6.0`,
a Rust toolchain, and `tinymist` (`sudo pacman -S tinymist`). The `typst` CLI is
optional; it's used to create projects from templates.

```sh
cargo run --release -- path/to/project     # or a .typ file
make install                               # to ~/.local (PREFIX=... to change)
```

If tinymist isn't on `$PATH`, set its location in *Preferences → Typst*, or
run with `PLARS_TINYMIST=/path/to/tinymist`. `PLARS_DEBUG=1` mirrors the
tinymist log panel to stderr.

## Features

| Edist (macOS) | This clone | How |
|---|---|---|
| Project folder sidebar, tabs | ✓ | `GtkDirectoryList` tree, `AdwTabView` |
| Syntax highlighting | ✓ | `data/language-specs/typst.lang` (markup / code / math modes) |
| Live preview, follows caret, lock to file, detach, partial rendering | ✓ | `tinymist.doStartPreview` (`--partial-rendering`) in WebKitGTK, throttled `tinymist.scrollPreview` (`panelScrollTo`; `changeCursorPosition` is disabled in tinymist 0.15), `tinymist.pinMain` |
| Click in preview → jump to source | ✓ | `window/showDocument` / `tinymist/preview/scrollSource` |
| Diagnostics + Problems list | ✓ | `textDocument/publishDiagnostics` |
| Completion, hover docs, go to definition, format | ✓ | LSP (`completion` with snippets, `hover`, `definition`, `formatting` via typstyle) |
| Outline ("On This Page") | ✓ | `textDocument/documentSymbol` |
| Export PDF / PNG / SVG (+ HTML, Markdown) | ✓ | `tinymist.export*` |
| Find / replace in file and in project | ✓ | `GtkSourceSearchContext`, regex over project files |
| New project from Typst Universe template | ✓ | `typst init` (fallback `tinymist.doInitTemplate`) |
| Typst documentation window | ✓ | WebKit on typst.app/docs |
| Keymaps: Plars, VS Code, JetBrains, Sublime Text, Emacs, Vim | ✓ | `keymap.rs` tables with multi-key sequences (`Ctrl+K Ctrl+C`, `C-x C-s`); Vim via `GtkSourceVimIMContext` |
| Multiple carets | ✓ | add next match / all matches, carets above/below, Alt+Click, split selection; typing, completion, paste and line commands apply at every caret |
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
  preview.rs    WebKit preview pane
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
