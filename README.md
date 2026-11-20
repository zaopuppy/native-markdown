# Native Markdown

A quiet, reader-first Markdown desktop app built with Rust and GPUI Component. Windows uses one system WebView2 control for the reading area; the window, file tree, source editor and file lifecycle remain native Rust. No Electron, bundled Chromium, Node runtime or frontend build is required.

## Features

- **Reader-first preview**: Preview is the default, with optional split and source-only modes
- **Preview and source modes**: Switch between preview, split, and source-only layouts
- **Browser-quality reading**: Selectable text, tables (including safe HTML spans), code highlighting, KaTeX math, local images, links and document zoom
- **Offline Mermaid diagrams**: Mermaid.js creates inline SVG with zoom, Fit, optional drag-to-pan and selectable labels. Diagrams render when near the viewport; individual failures show their source
- **Folder tree**: Lazy, read-only Markdown browsing rooted at the document folder, with parent and folder-root navigation
- **Document outline**: Resizable right sidebar, default Jump navigation and optional section Focus
- **Find**: Search rendered text and jump between matching sections
- **Safe file lifecycle**: Atomic saves, Save As, dirty-state warnings, visible errors, and background recovery
- **Desktop integration**: Native dialogs, drag-and-drop, and command-line paths
- **Local-image safety**: Relative images are restricted to the active document directory; remote images are disabled by default
- **Reading metadata**: Word count and estimated reading time
- **Appearance settings**: Open the toolbar gear to choose Paper, Sepia, Sage, Midnight or Nord. The app, source editor, reading surface and Mermaid diagrams share the selected theme; the choice is saved across restarts.

## Shortcuts

| Action | Shortcut |
| --- | --- |
| Open | `Ctrl+O` |
| Save | `Ctrl+S` |
| Save As | `Ctrl+Shift+S` |
| Find | `Ctrl+F` |
| Toggle preview/editing | `Ctrl+E` |
| Preview / Split / Source | `Ctrl+1` / `Ctrl+2` / `Ctrl+3` |
| Zoom in / out / reset | `Ctrl++` / `Ctrl+-` / `Ctrl+0` |

Use `Ctrl+mouse-wheel` for document zooming, or inside a diagram for independent SVG zooming. Toggle **Pan** before dragging a diagram; leave Pan off to select and copy its text. Click a local image to open its zoomable viewer.

Windows and Microsoft Edge WebView2 Runtime are required for the reading surface. Initialization failure shows an explicit error; source editing and saving remain available. There is no native preview fallback or renderer selection environment variable, and the app does not install a runtime silently. Other platforms do not yet have a supported reading backend.

Relative local images are restricted to the active document folder. Remote images are disabled. See [the rendering evaluation](docs/markdown-rendering-evaluation.md) for historical process-tree memory measurements and the final architecture.

When the file tree is focused, use `Up` / `Down` to move, `Left` / `Right` to collapse or expand folders, and `Enter` to open a Markdown file.

## Mermaid and reading interaction

Offline Mermaid.js renders inline SVG with selectable labels, independent zoom, Fit and optional drag-to-pan. Normal text selection works with Pan disabled. A broken diagram shows its error without blocking the document. The reading surface, code blocks and diagrams use narrow rounded scrollbars without arrow buttons.

Untrusted Markdown HTML is sanitized. Scripts, runtime diagram configuration, external diagram resources and arbitrary Rust IPC are blocked. Each document allows up to 64 diagrams, each with at most 256 KiB of source and 1,500 edges. Mermaid frontmatter is currently rejected, including title-only frontmatter.

## Building

```bash
cargo build --release
```

The binary will be at `target/release/native-markdown.exe` on Windows.

## Running

```bash
cargo run
# Open a document directly
cargo run -- path/to/document.md
# or directly:
./target/release/native-markdown
```

## Memory regression benchmark

The measurement script records the application and its WebView2 descendants, including process-level Working Set, Private Working Set, Private Bytes and CPU. Shared working sets must not be treated as exclusive memory:

```powershell
python scripts/make-rendering-fixtures.py
.\scripts\measure-rendering.ps1 -Binary target\release\native-markdown.exe -Runs 3 -Seconds 12 -Output tmp\webview.json
python scripts/summarize-rendering.py tmp\webview.json
```

The earlier native budget harness and renderer prototypes have been retired. Historical comparison results remain in `docs/measurements`; they are not measurements of every later build.

## Verification

```powershell
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
uv run --with playwright python scripts/test-web-preview.py --binary target/release/native-markdown.exe
python scripts/test-delayed-web-preview.py --binary target/release/native-markdown.exe --runs 3
```

The first suite checks native editor, file lifecycle, analysis and sanitized HTML projection. The latter scripts exercise the actual WebView2 process and delayed-open lifecycle; no native reading implementation substitutes for those tests.

## Tech Stack

- **GPUI**: GPU-accelerated native window and rendering runtime
- **GPUI Component 0.10.0**: Official crates.io release, with gpui-pre/platform 0.3.12; application-owned search integration and no local fork. See [upgrade notes](docs/gpui-component-fork.md)
- **Wry + system WebView2**: One Windows reading surface, with no bundled browser engine
- **pulldown-cmark + ammonia**: HTML projection, safety, outline, search and metadata
- **Offline JavaScript assets**: Mermaid, KaTeX, highlight.js and DOMPurify, with pinned versions and licenses
- **rfd**: Native file dialogs
