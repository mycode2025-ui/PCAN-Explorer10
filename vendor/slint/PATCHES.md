# Slint vendor patches

Upstream baseline: Slint 1.18.0 crates from crates.io.

Only `i-slint-core` and `i-slint-backend-winit` are overridden from the root
`Cargo.toml`.

## Active local patches

1. `i-slint-backend-winit/winitwindowadapter.rs`
   - On Windows, invalidate occlusion state and request a redraw after restore,
     move, focus, and unocclude events.
   - This prevents stale or transparent regions when DWM replaces a software
     surface while a window is partly off-screen or moved between monitors.
2. `i-slint-backend-winit/renderer/sw.rs`
   - On Windows, always use `RepaintBufferType::NewBuffer` for the softbuffer
     target instead of trusting buffer age.
   - Other platforms retain upstream buffer-age reuse.

## Patch absorbed by Slint 1.18

The former `i-slint-core/textlayout/sharedparley.rs` patch clipped large
multi-line selections to the visible TextInput viewport. Slint 1.18 implements
this upstream through `draw::visible_band()` and `SelectionSpans`, so the old
local implementation and its test are intentionally not carried forward.

## Upgrade verification

Compare each vendored crate against the matching crates.io source. For 1.18.0,
`i-slint-core` must have no differences and `i-slint-backend-winit` must differ
only in the two active patch files above.
