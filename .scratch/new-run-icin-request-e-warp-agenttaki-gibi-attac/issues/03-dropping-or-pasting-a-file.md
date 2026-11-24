# 03: Dropping or pasting a file attaches it too

**Status:** ready-for-agent
**Category:** enhancement
**Blocked by:** 02

## What to build

The two attach gestures warp-agent's composer also has, both landing in the
strip task 02 draws and both going through `attach_new_run_paths`:

- **Drop.** The card (`#flow-new-run-card` in `render_new_run`) accepts files
  dragged from the file manager:
  `.drag_over::<gpui::ExternalPaths>(|style, _, _, _| style.border_color(theme::accent()))`
  for the affordance, and
  `.on_drop(cx.listener(|this, paths: &gpui::ExternalPaths, _, cx| this.attach_new_run_paths(paths.paths().to_vec(), cx)))`.
  The card is the target so the field, the strip and the folder line all accept
  a drop; the scrim outside it still closes the form.
- **Paste.** The request area gets a paste hook. `warpui_kit_core::field::area`
  seats its `Textarea` internally and exposes no hook, so seat the box from the
  same public pieces it is made of — `field::boxed` and `field::bare_area` —
  and mirror `field::area`'s 9/10 padding in one commented place:

  ```rust
  let focus = request.read(cx).focus_handle(cx);
  field::boxed("flow-new-run-request", &focus, window, px(theme::scaled(140.)))
      .min_h(px(0.))
      .flex_col()
      .pt(px(theme::scaled(9.)))
      .pb(px(theme::scaled(9.)))
      .px(px(theme::scaled(6.)))
      .child(field::bare_area(&request).h_full().on_paste(handler))
  ```

  The comment names `warpui_kit_core::field::area` as the shape being mirrored
  and why (the paste hook is the one thing that function cannot give). The
  `flow-new-run-request` debug selector is unchanged, so the existing probes
  keep clicking and typing into it.

  `on_paste`'s handler (`Fn(&gpui::ClipboardItem, &mut Window, &mut App) ->
  bool`) is built with `let flow = cx.entity();` and returns:
  - `true` for a `gpui::ClipboardEntry::Image` after
    `flow.update(cx, |this, cx| this.attach_new_run_image(image.bytes.clone(), cx))`;
  - `true` for a `gpui::ClipboardEntry::ExternalPaths` after
    `flow.update(cx, |this, cx| this.attach_new_run_paths(paths.paths().to_vec(), cx))`;
  - `false` otherwise, so text still pastes into the field.

- `fn attach_new_run_image(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>)`:
  returns when the form is closed; names it `paste-<n>.png` with `n =
  form.attachments.len() + 1`, made unique with `unique_attachment_name` against
  the strip's names; `mime = "image/png"`; `image =
  warpui_kit_core::decode_png_render(&bytes)`; push, clear `error`, `cx.notify()`.
- `#[doc(hidden)] pub fn new_run_request_for_probe(&self, cx: &gpui::App) ->
  Option<String>`: the request's text, read the way `board_probe.rs:596` reads
  another field's value, so the paste probe can prove no text was inserted.

`tests/new_run_probe.rs` — new `#[gpui_kit::gpui::test]`
`new_run_pastes_an_image_into_the_request` (its own `hold_home()` and
`scratch_home`):

- seed the repo, open the window, `click(cx, "flow-runs-new")`;
- `cx.write_to_clipboard(gpui::ClipboardItem::new_image(&gpui::Image {
  format: gpui::ImageFormat::Png, bytes: TINY_PNG.to_vec(), id: 1 }))` where
  `TINY_PNG` is a 1×1 PNG byte constant in the probe file;
- `cx.update(|window, cx| window.dispatch_action(Box::new(
  gpui_kit::component::input::Paste), cx))`; `settle(cx)`;
- assert `page.read_with(cx, |page, _| page.new_run_attachments_for_probe())
  == vec!["paste-1.png".to_owned()]`, the chip
  `flow-new-run-attachment-paste-1.png` has bounds, and
  `page.read_with(cx, |page, cx| page.new_run_request_for_probe(cx))` is
  `Some(String::new())`;
- then type a request, Create, and assert
  `.scratch/<slug>/attachments/paste-1.png` holds `TINY_PNG` byte for byte.

If `dispatch_action` does not reach the field, `cx.simulate_keystrokes("ctrl-v")`
with the request focused is the fallback: the point is that a clipboard image
becomes an attachment and no text is inserted, not which entry point fired.

## Where it lives

- `src/ui/new_run.rs` — `open_new_run` 313–340 (nothing changes);
  `render_new_run` 392–557: the request area at 421–425 and the card at
  463–488 (`#flow-new-run-card`'s handlers at 466–478); `Attachment`,
  `read_attachment` and `unique_attachment_name` from task 01;
  `attach_new_run_paths` and `adopt_new_run_attachments` from task 02.
- The pinned kit, read only, in the sibling checkout —
  `../warp-suite/kits/warpui-kit-core/src/field.rs`: `boxed` 48–87, `area`
  189–204, `bare_area` 240–250; and `Textarea::on_paste` in
  `gpui-component-0.9.0/src/input/textarea.rs:163` (the kit's documented hook:
  `true` consumes the paste, `false` falls through to text). Do not change the
  kit.
- `tests/new_run_probe.rs` — the pattern at 99–177 and the helpers at 43–97.

## Done when

- `cargo test --locked --test new_run_probe` passes, including
  `new_run_pastes_an_image_into_the_request` and the task 02 scenario.
- The paste probe proves: one chip named `paste-1.png`, no text inserted, and
  the bytes on disk after Create.
- The drop handler is on `#flow-new-run-card` as `.on_drop::<ExternalPaths>`
  calling `attach_new_run_paths`; a headless probe cannot simulate an external
  drop, so the compiler and that shared call are its proof, and the picker
  probe covers the function it calls.
- `cargo fmt --all --check` and
  `cargo clippy --locked --all-targets -- -D warnings` pass; no engine or kit
  file changes.
