# Standalone Windows reader PoC

This Winit/Wry window was used before integrating a child WebView into GPUI. It shares
`src/web_document.rs` and the offline web assets with the application. It intentionally
does not implement the native file tree, editor, saving or document-switch commands.
Do not compare its resource usage as if it were the complete application.

```powershell
cargo run --release --manifest-path spikes/webview-reader/Cargo.toml -- tests/fixtures/mermaid-smoke.md
uv run --with playwright python scripts/test-web-preview.py --binary spikes/webview-reader/target/release/webview-reader-poc.exe --poc
```

See [the evaluation](../../docs/markdown-rendering-evaluation.md) for process-tree
measurements and the reason for retaining the native GPUI shell.
