# Pixelferrite

A layered image editor in Rust for macOS and Linux.

```sh
cargo run --release -p pixelferrite            # empty canvas
cargo run --release -p pixelferrite -- photo.jpg
```

Linux needs the usual winit/wgpu system packages (X11 or Wayland dev libraries
and a Vulkan or GL driver); file dialogs go through the XDG desktop portal.

## Layout

- `crates/pf-core` — document model, compositor, tools and file formats. No
  GUI dependencies; everything is covered by headless tests.
- `crates/pixelferrite` — the egui/eframe application.

`cargo test` runs the core tests plus a headless UI test that drives the real
app with synthetic pointer input and writes rendered frames to `target/uitest/`.

## Files

The native format is [OpenRaster](https://www.openraster.org/) (`.ora`), which
Krita, GIMP and MyPaint also open. Layer masks are stored as extra PNGs inside
the archive that other apps ignore. Export writes PNG, JPEG or GIF. Undo
history lives in memory for the session and is not written to the file.

## Controls

| | |
|---|---|
| Pinch / Ctrl-scroll | Zoom |
| Two-finger rotate / Alt-scroll | Rotate the canvas |
| Scroll, Space-drag, middle-drag | Pan |
| `[` `]` | Brush size |
| `X` / `D` | Swap / reset colors |
| Shift / Alt / both while selecting | Add / subtract / intersect |
| Alt-click (clone stamp) | Set clone source |
| Alt-click (brush, pencil) | Pick color |

Tool keys: V arrange, M / O / L rectangle, ellipse and free selection, Q quick
selection, W magic wand, B brush, N pencil, E eraser, G gradient, K fill,
S clone stamp, R smudge, I color picker, H hand, Z zoom.
