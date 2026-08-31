# Renderer spike results

> PROTOTYPE result captured on 2026-08-31. These numbers are comparative,
> machine-specific measurements, not product guarantees.

## Input

`E:\data_z\BOOK\ing\Build-A-Large-Language-Model-CN-main\cn-Book\1.理解大语言模型.md`

- Markdown source: 29,224 bytes
- HTML pattern exercised: centered `div` containing local PNG `img` with
  percentage width
- Release builds, 900 × 760 window

## Reproduction

```powershell
.\spikes\bench.ps1 all "E:\data_z\BOOK\ing\Build-A-Large-Language-Model-CN-main\cn-Book\1.理解大语言模型.md" -Seconds 3
.\spikes\bench.ps1 egui "E:\data_z\BOOK\ing\Build-A-Large-Language-Model-CN-main\cn-Book\1.理解大语言模型.md" -Seconds 3 -AutoScroll
```

## Measurements

| Renderer | Pass | First render | RSS after 3 s | Peak RSS | Callback interval p95 | Release binary | Locked packages |
|---|---:|---:|---:|---:|---:|---:|---:|
| `egui_commonmark_extended 0.25` | idle | 120.0 ms | 425.4 MiB | 425.4 MiB | 85.4 ms at 100 ms sampling repaint | 8.14 MiB | 345 |
| `egui_commonmark_extended 0.25` | auto-scroll | 128.7 ms | 427.0 MiB | 427.0 MiB | 18.6 ms | 8.14 MiB | 345 |
| `gpui-component 0.5.1` | idle/first screen, before local-image fix | 236.9 ms | 107.7 MiB | 107.7 MiB | not available | 16.03 MiB | 759 |

The first GPUI row did not load local images and is retained only as a record;
it is not comparable with egui. A corrective validation run loaded two images
on the first screen and ended at 125.2 MiB RSS after 3 seconds. Its cold first
frame was 458.8 ms and the release binary was 16.09 MiB. A corrected egui run,
after fixing HTML images being constrained by the remaining vertical layout
space, ended at 437.8 MiB RSS; image sizing did not address its eager cache.

The callback statistic is the interval between UI callbacks, not isolated CPU
render duration. In the idle egui pass the prototype intentionally requests a
repaint every 100 ms so it can sample and exit; that p95 therefore does not
represent a missed-frame problem.

## Corrected comparison setup

The 2026-08-31 follow-up added a Chinese fallback font to both egui font
families and matched wheel distance to GPUI's Windows behavior: the system
setting is 3 lines per notch and GPUI converts each line to 20 points, so both
prototypes now travel 60 points per physical wheel notch. Framework-native
scroll animation/frame pacing remains unchanged because it is part of the
experience under comparison.

| Renderer | Pass | First callback | RSS after 3 s | Peak RSS | Images loaded |
|---|---:|---:|---:|---:|---:|
| `egui_commonmark_extended 0.25` | idle, CJK enabled | 151.8 ms | 479.5 MiB | 479.5 MiB | eager/default pipeline |
| `gpui-component 0.5.1` | idle/first screen | 309.7 ms | 138.6 MiB | 154.8 MiB | 2 |

These idle numbers do not replace a manual full-document scroll pass. Loading
the 19 MiB Microsoft YaHei collection raised egui RSS, which is expected and
removes the earlier unfair condition where egui displayed missing glyphs.

## What the spike answered

1. `egui_commonmark_extended::show_scrollable` is fast enough to reach a first
   frame under the 300 ms target on this chapter.
2. Its default image loaders eagerly retain enough decoded/image/texture state
   to push RSS above 420 MiB even without scrolling. Viewport-clipped Markdown
   layout is therefore not viewport-bounded image memory.
3. `gpui-component::TextView` stays below the 160 MiB hard limit on the first
   screen after two local images load and appears to defer offscreen work, but
   its cold first render is slower and its binary/dependency footprint is
   roughly double the egui spike.
4. The GPUI number is not yet a valid scrolling result. `TextView` keeps the
   virtual list state private, Windows Computer Use was unavailable during the
   run, and the prototype therefore could not programmatically traverse all
   images. A manual scroll pass remains necessary before treating 125.1 MiB as
   representative of the full document.

## Decision

Do not migrate the application to GPUI based on this result. Keep the existing
UI stack for now and treat the Markdown component and image pipeline as separate
choices:

- reuse `egui_commonmark_extended` (or upstream `egui_commonmark`) for parsing,
  Markdown layout, and viewport clipping;
- replace the default image path with the planned application-owned loader that
  downscales before upload and enforces the 48 MiB soft / 160 MiB hard budgets;
- repeat the same benchmark after that loader exists;
- keep the GPUI prototype as a comparison until a real manual full-document
  scroll confirms its peak RSS and HTML/image correctness.
