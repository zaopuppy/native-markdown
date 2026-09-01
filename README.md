# Native Markdown

A quiet, reader-first Markdown desktop app built with Rust and GPUI Component. Documents open in a native, virtualized preview without a browser or WebView.

## Features

- **Reader-first preview**: Preview is the default, with optional split and source-only modes
- **Preview and source modes**: Switch between preview, split, and source-only layouts
- **Native Markdown rendering**: Headings, tables, task lists, strikethrough, links, local images, highlighted code blocks, and simple inline HTML
- **Offline Mermaid diagrams**: Fenced `mermaid` blocks render through an isolated native worker, with no browser, WebView, Node, or network service
- **Folder tree**: Lazy, read-only Markdown browsing rooted at the document folder, with parent and folder-root navigation
- **Document outline**: Collapsible heading navigation in a resizable right sidebar
- **Find**: Search rendered text and jump between matching sections
- **Safe file lifecycle**: Atomic saves, Save As, dirty-state warnings, visible errors, and background recovery
- **Desktop integration**: Native dialogs, drag-and-drop, and command-line paths
- **Local-image safety**: Relative images are restricted to the active document directory; remote images are disabled by default
- **Bounded image cache**: 48 MiB soft budget, temporary overage while scrolling, idle LRU cleanup, and a 160 MiB hard limit; no disk cache
- **Reading metadata**: Word count and estimated reading time

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

Use `Ctrl+mouse-wheel` for smooth zooming. Set `NATIVE_MARKDOWN_REMOTE_IMAGES=1` before launch only when a document is allowed to load remote images.

When the file tree is focused, use `Up` / `Down` to move, `Left` / `Right` to collapse or expand folders, and `Enter` to open a Markdown file.

## Mermaid compatibility

Mermaid rendering uses the pinned native `merman 0.7.0` renderer and targets Mermaid 11.15 syntax. Flowchart, sequence, class, state, ER, Gantt, pie, mindmap, timeline, journey, GitGraph, C4, block, packet, radar, treemap, XYChart, architecture, requirement, quadrant, Sankey, and Kanban diagrams are supported. TreeView, Ishikawa, Event Modeling, and Venn rendering is experimental.

For untrusted documents, Mermaid runtime configuration, HTML labels other than plain `<br>` line breaks, click handlers, links, and external resources are disabled. Only a frontmatter `title` is accepted. A block is limited to 256 KiB, a document to 64 diagrams, and generated images are rendered within bounded SVG and pixel budgets. Browser-pixel-identical layout is not guaranteed.

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

The Windows benchmark launches a fresh real application process for each scenario, records working set, private working set, and private bytes, then fails when a configured budget is exceeded:

```powershell
.\scripts\benchmark-memory.ps1 `
  'path\to\document.md' `
  -Scenario all `
  -Seconds 5
```

Use `-Scenario zoom -Steps 5 -StepMs 300` for the minimal staged-zoom probe. The `scroll` scenario sends real wheel messages to the benchmark window. Defaults are 160 MiB peak private working set, 160 MiB peak private bytes, and 80 MiB private-byte growth; override them with `-MaxPrivateWorkingSetMiB`, `-MaxPrivateBytesMiB`, and `-MaxGrowthMiB`.

## Tech Stack

- **GPUI**: GPU-accelerated native window and rendering runtime
- **GPUI Component fork**: Two pinned generic `TextView` seams; see [fork maintenance](docs/gpui-component-fork.md)
- **GPUI Component `TextView`**: Virtualized Markdown, simple HTML, and image rendering
- **pulldown-cmark**: Outline, search, and reading metadata analysis
- **rfd**: Native file dialogs
