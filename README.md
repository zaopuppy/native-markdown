# Native Markdown

A quiet, reader-first Markdown desktop app built with Rust and egui. Documents open in a polished native preview; editing tools appear only when you ask for them.

## Features

- **Reader-first preview**: Preview is the default, with optional split and source-only modes
- **Native GFM rendering**: Headings, tables, task lists, strikethrough, links, local images, footnotes, and highlighted code blocks
- **Document outline**: Collapsible heading navigation for long documents
- **Find**: Search rendered text or highlight matches in the source editor
- **Safe file lifecycle**: Atomic saves, Save As, dirty-state warnings, visible errors, and background recovery
- **Desktop integration**: Native dialogs, drag-and-drop, command-line paths, and last-document reopening
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

Links require `Ctrl+Click` so reading never opens a browser accidentally.

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

## Tech Stack

- **egui**: Immediate mode GUI library
- **egui_commonmark / pulldown-cmark**: Native Markdown parsing and rendering
- **rfd**: Native file dialogs
