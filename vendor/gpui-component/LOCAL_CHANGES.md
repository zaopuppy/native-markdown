# Local component patch

Source: https://github.com/zaopuppy/gpui-component
Revision: 46f6b9c41f7db3a41228ca8c15b4680356080f19 (0.5.1)
License: LICENSE-APACHE

Only the UI and macro crates are included. Cargo uses this checked-in copy so a
fresh checkout builds the same renderer, without modifying Cargo's shared cache.

Local changes:

- `text/search.rs`: Unicode match ranges, marked snippet text, and render geometry.
- `text/node.rs`: locate matches in render order; prepare inline ranges before
  virtualization; reveal source alongside a custom code renderer for source hits.
- `text/inline.rs`: paint explicit backgrounds, keep formatted spans from repainting
  over active matches, and record measured match bounds and colors.
- `text/text_view.rs`: `search_match` API, scroll to the containing list item, then
  center the measured text line once per navigation request.
- `input/state.rs`, `input/element.rs`: external search highlighting and centering
  without changing the user's selection or stealing input focus.

App-level GPUI regression tests in `src/app.rs` exercise keyboard dispatch,
painted geometry, scrolling, and search lifetime through the actual renderer.
