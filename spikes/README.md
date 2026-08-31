# Markdown renderer spikes

> PROTOTYPE — disposable comparison code, not production architecture.

These two Rust applications load the same Markdown file so native renderers can
be compared without changing the main application:

- `egui-extended`: `egui_commonmark_extended::show_scrollable`
- `gpui-text-view`: `gpui_component::text::TextView::scrollable(true)`

Run either prototype from the repository root:

```powershell
.\spikes\run.ps1 egui "E:\path\chapter.md"
.\spikes\run.ps1 gpui "E:\path\chapter.md"
```

Both applications print `MARKDOWN_SPIKE_METRIC` records to stdout and show the
same measurements in their header. The egui spike also supports an automated
scroll pass:

```powershell
$env:MARKDOWN_SPIKE_AUTOSCROLL = "1"
$env:MARKDOWN_SPIKE_SECONDS = "10"
.\spikes\run.ps1 egui "E:\path\chapter.md"
```

For a timed, reproducible pass that exits by itself:

```powershell
.\spikes\bench.ps1 all "E:\path\chapter.md" -Seconds 5
.\spikes\bench.ps1 egui "E:\path\chapter.md" -Seconds 10 -AutoScroll
```

GPUI's `TextView` keeps its `ListState` private, so the standalone prototype
cannot drive its scroll position programmatically. Its timed pass is therefore
an idle/first-screen measurement; use `run.ps1` for a manual scroll stress test.
On Windows, both prototypes use the system wheel-lines setting and GPUI's list
conversion of 20 points per line, so one physical wheel notch travels the same
total distance. Their animation/frame pacing is intentionally left native to
each framework because that behavior is part of the experience being compared.
The GPUI prototype supplies a document-rooted local image client because
`TextView` otherwise sends relative image references to its HTTP client. The
header shows how many local image requests completed. Remote images remain
disabled unless `MARKDOWN_SPIKE_REMOTE_IMAGES=1` is set explicitly.

The prototype deliberately does not write a document or image cache to disk.
Cargo artifacts share the repository's ignored root `target` directory.
