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
the archive that other apps ignore, and text layers carry their text, font and
path as extra attributes so they stay editable (other apps see the rendered
pixels). Export writes PNG, JPEG or GIF. Undo history lives in memory for the
session and is not written to the file.

## Text

With the type tool (T), click to place text or drag to draw a path that new
text follows. Click existing text to edit it; its options are in the inspector.
"Redraw Path" gives existing text a new path and "Straighten" removes it.
Painting on a text layer, or filtering it, turns it into ordinary pixels.

## Filters

Filters live in the Filter menu and preview on the canvas until you press
Apply. They affect the active layer (or its mask when that is being edited),
limited to the selection if there is one. To add one, add a variant to
`Filter` in `crates/pf-core/src/filter.rs` and its controls to
`App::filter_dialog`.

- **Gaussian Blur**
- **Edge Detection** — Canny edge detection (the same algorithm as OpenCV's
  `Canny`, implemented here so there is no native library to install). Draws
  the edges as lines on white or black, or highlights them over the image;
  "Select Edges Instead" turns them into a selection.

## AI edits

Layer > "Send to AI with Prompt…" (also in a layer's right-click menu) asks
for a prompt and sends the active layer to OpenAI's image model; the answer
comes back as a new layer above it. With a selection, only the selected area
(plus some surroundings for context) is sent, and only the selection is
replaced, scaled to fit.

The prompt dialog shows exactly what will be sent. Every request is kept, with
the image that was sent, the mask and the answer: Layer > "AI Requests…" lists
them and can add an old result back as a layer or reuse its prompt.

Set the key in File > Settings, or in a `.env` file in the directory you run
from (or any parent). Each setting is taken from `.env` first, then the
settings file, then the shell environment:

```
OPENAI_API_KEY=sk-...
# optional
OPENAI_IMAGE_MODEL=gpt-image-2
OPENAI_IMAGE_QUALITY=medium
```

`.env` is git-ignored. `cargo test -p pixelferrite -- --ignored live` sends two
small real requests to check the key and model.

## Inserting images

File > "Insert Image as New Layer…" adds an image centred on the canvas (so
does dropping a file on the window). "Insert Image into Selection…" scales
it to fill the selection, keeping its proportions, and cuts it to the
selection's shape, as a new layer.

## Saved data

```
~/.config/pixelferrite/            ($XDG_CONFIG_HOME)
  config.toml                      settings; owner-readable only, holds the AI key
~/.local/share/pixelferrite/       ($XDG_DATA_HOME)
  recent.json                      File > Open Recent
  ai/<date>-<time>-<id>/           one folder per AI request
    request.json                   prompt, model, size, layer, area, status, timing, usage
    input.png  mask.png  output.png
```

`config.toml`:

```toml
[ai]
api_key = "sk-..."
model = ""          # empty = gpt-image-2
quality = ""        # low | medium | high, empty = automatic
base_url = ""       # empty = OpenAI
keep_history = 200  # AI requests to keep
```

`PIXELFERRITE_CONFIG_DIR` and `PIXELFERRITE_DATA_DIR` move the two folders.
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
S clone stamp, R smudge, T type, I color picker, H hand, Z zoom.
