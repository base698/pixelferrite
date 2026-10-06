//! The canvas: view gestures, tool interaction and overlays.

use std::sync::Arc;

use egui::{Color32, CursorIcon, Mesh, Modifiers, PointerButton, Pos2, Rect, Sense, Shape, Stroke as Line, Ui, Vec2, pos2};
use pf_core::fill::{self, GradientOp, GradientParams};
use pf_core::paint::{PaintKind, Stroke};
use pf_core::selection::{self, Combine};
use pf_core::{DocState, IRect, Pixmap, composite};

use crate::app::App;
use crate::tools::{GradientFill, Tool};
use crate::view::View;

/// What the pointer is doing between press and release.
pub enum Drag {
    None,
    Pan,
    ZoomScrub,
    Pick,
    Stroke(Stroke),
    Gradient { op: GradientOp, start: Pos2, end: Pos2 },
    Move { before: DocState, start: Pos2, orig: (i32, i32), moved: bool },
    Marquee { start: Pos2, cur: Pos2, mode: Combine, ellipse: bool },
    Lasso { pts: Vec<Pos2>, mode: Combine },
    Quick { before: DocState, src: Pixmap, mode: Combine, last: Pos2 },
}

/// Cached selection outline, rebuilt when the selection mask changes.
#[derive(Default)]
pub struct Ants {
    key: usize,
    segs: Vec<[i32; 4]>,
}

const ACCENT: Color32 = Color32::from_rgb(64, 140, 255);

/// Held modifiers win; otherwise the mode picked in the inspector applies.
fn combine_mode(m: Modifiers, default: Combine) -> Combine {
    match (m.shift, m.alt) {
        (true, true) => Combine::Intersect,
        (true, false) => Combine::Add,
        (false, true) => Combine::Subtract,
        _ => default,
    }
}

pub fn canvas(app: &mut App, ui: &mut Ui) {
    let rect = ui.available_rect_before_wrap();
    let resp = ui.interact(rect, ui.id().with("canvas"), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let ctx = ui.ctx().clone();

    app.view.vp = rect;
    app.view.ppp = ctx.pixels_per_point();
    if app.fit_pending && rect.width() > 1.0 {
        app.view.fit(app.doc.state.width, app.doc.state.height);
        app.fit_pending = false;
    }

    // ---- input snapshot ----
    let inp = ui.input(|i| {
        let moves: Vec<Pos2> = i
            .events
            .iter()
            .filter_map(|e| if let egui::Event::PointerMoved(p) = e { Some(*p) } else { None })
            .collect();
        Input {
            pos: i.pointer.latest_pos(),
            moves,
            delta: i.pointer.delta(),
            pressed: i.pointer.button_pressed(PointerButton::Primary),
            down: i.pointer.button_down(PointerButton::Primary),
            mid_pressed: i.pointer.button_pressed(PointerButton::Middle),
            mid_down: i.pointer.button_down(PointerButton::Middle),
            mods: i.modifiers,
            space: i.key_down(egui::Key::Space),
            scroll: i.smooth_scroll_delta() + i.multi_touch().map_or(Vec2::ZERO, |t| t.translation_delta),
            zoom: i.zoom_delta(),
            rot: i.rotation_delta(),
            time: i.time,
        }
    });
    let over = resp.contains_pointer();

    // ---- view gestures: pinch to zoom, two-finger rotate, scroll to pan ----
    if over {
        let anchor = inp.pos.unwrap_or(rect.center());
        if inp.zoom != 1.0 {
            app.view.zoom_about(anchor, inp.zoom);
        }
        if inp.rot != 0.0 {
            app.view.rotate_about(anchor, inp.rot);
            app.last_rotate = inp.time;
        }
        if inp.scroll != Vec2::ZERO {
            if inp.mods.alt {
                app.view.rotate_about(anchor, inp.scroll.y * 0.004);
                app.last_rotate = inp.time;
            } else {
                app.view.pan(inp.scroll);
            }
        }
    }
    // Settle back to upright when a rotation gesture ends near 0 degrees.
    if app.view.rot != 0.0 && app.view.rot.abs() < 4f32.to_radians() {
        if inp.time - app.last_rotate > 0.3 {
            let r = app.view.rot;
            app.view.rotate_about(rect.center(), -r);
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    // ---- tool interaction ----
    let on_canvas = resp.is_pointer_button_down_on() || over;
    if matches!(app.drag, Drag::None) && on_canvas {
        if inp.mid_pressed || (inp.pressed && (inp.space || app.tool == Tool::Hand)) {
            app.drag = Drag::Pan;
        } else if inp.pressed {
            if let Some(p) = inp.pos {
                press(app, app.view.to_doc(p), inp.mods);
            }
        }
    } else if !matches!(app.drag, Drag::None) {
        let still_down = if matches!(app.drag, Drag::Pan) { inp.down || inp.mid_down } else { inp.down };
        let mut pts = inp.moves.clone();
        if pts.is_empty() {
            pts.extend(inp.pos);
        }
        for p in pts {
            dragged(app, app.view.to_doc(p), p, &inp);
        }
        if !still_down {
            release(app);
        }
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        cancel(app);
    }

    // ---- draw ----
    let dirty = app.doc.take_dirty();
    app.display.sync(&ctx, &app.doc.state, dirty);
    let (w, h) = (app.doc.state.width as f32, app.doc.state.height as f32);
    let quad = |r: Rect, v: &View| [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].map(|p| v.to_screen(p)).to_vec();
    let canvas_quad = quad(Rect::from_min_max(Pos2::ZERO, pos2(w, h)), &app.view);
    painter.add(Shape::convex_polygon(
        canvas_quad.iter().map(|p| *p + Vec2::splat(3.0)).collect(),
        Color32::from_black_alpha(90),
        Line::NONE,
    ));
    app.display.paint(&painter, &app.view);

    // Selection outline ("marching ants").
    if let Some(sel) = &app.doc.state.selection {
        let key = Arc::as_ptr(sel) as usize;
        if app.ants.key != key {
            app.ants = Ants { key, segs: selection::outline(sel) };
        }
        draw_ants(&painter, &app.view, &app.ants.segs, inp.time);
        ctx.request_repaint_after(std::time::Duration::from_millis(120));
    } else {
        app.ants = Ants::default();
    }

    // Active layer bounds for the arrange tool.
    if app.tool == Tool::Move {
        if let Some(l) = app.doc.state.active_layer() {
            let r = l.rect();
            let r = Rect::from_min_max(pos2(r.x0 as f32, r.y0 as f32), pos2(r.x1 as f32, r.y1 as f32));
            painter.add(Shape::closed_line(quad(r, &app.view), Line::new(1.0, ACCENT)));
        }
    }

    // In-progress shapes.
    let contrast = |painter: &egui::Painter, pts: Vec<Pos2>, closed: bool| {
        for (wd, col) in [(2.5, Color32::from_black_alpha(160)), (1.0, Color32::WHITE)] {
            let s = Line::new(wd, col);
            painter.add(if closed { Shape::closed_line(pts.clone(), s) } else { Shape::line(pts.clone(), s) });
        }
    };
    match &app.drag {
        Drag::Marquee { start, cur, ellipse, .. } => {
            let r = Rect::from_two_pos(*start, *cur);
            let pts = if *ellipse {
                (0..72)
                    .map(|i| {
                        let a = i as f32 / 72.0 * std::f32::consts::TAU;
                        app.view.to_screen(r.center() + Vec2::new(a.cos() * r.width(), a.sin() * r.height()) * 0.5)
                    })
                    .collect()
            } else {
                quad(r, &app.view)
            };
            contrast(&painter, pts, true);
        }
        Drag::Lasso { pts, .. } => {
            contrast(&painter, pts.iter().map(|p| app.view.to_screen(*p)).collect(), false);
        }
        Drag::Gradient { start, end, .. } => {
            let (a, b) = (app.view.to_screen(*start), app.view.to_screen(*end));
            contrast(&painter, vec![a, b], false);
            for p in [a, b] {
                painter.circle(p, 4.0, Color32::WHITE, Line::new(1.0, Color32::BLACK));
            }
        }
        _ => {}
    }

    // Clone source marker.
    if app.tool == Tool::Clone {
        let src = match (app.clone_off, app.clone_src, inp.pos) {
            (Some((dx, dy)), _, Some(p)) if over => {
                Some(app.view.to_screen(app.view.to_doc(p) + Vec2::new(dx as f32, dy as f32)))
            }
            (None, Some(s), _) => Some(app.view.to_screen(s)),
            _ => None,
        };
        if let Some(s) = src {
            for (wd, col) in [(3.0, Color32::from_black_alpha(160)), (1.0, Color32::WHITE)] {
                painter.line_segment([s - Vec2::X * 7.0, s + Vec2::X * 7.0], Line::new(wd, col));
                painter.line_segment([s - Vec2::Y * 7.0, s + Vec2::Y * 7.0], Line::new(wd, col));
            }
        }
    }

    // Cursor.
    if over || !matches!(app.drag, Drag::None) {
        let panning = inp.space || app.tool == Tool::Hand || matches!(app.drag, Drag::Pan);
        let mut icon = match app.tool {
            _ if panning => if inp.down || inp.mid_down { CursorIcon::Grabbing } else { CursorIcon::Grab },
            Tool::Move => CursorIcon::Move,
            Tool::Zoom => if inp.mods.alt { CursorIcon::ZoomOut } else { CursorIcon::ZoomIn },
            _ => CursorIcon::Crosshair,
        };
        if let (false, Some(size), Some(p)) = (panning, app.settings.cursor_size(app.tool), inp.pos) {
            let r = size * 0.5 * app.view.scale();
            if r > 5.0 {
                painter.circle_stroke(p, r, Line::new(2.0, Color32::from_black_alpha(140)));
                painter.circle_stroke(p, r, Line::new(1.0, Color32::WHITE));
                icon = CursorIcon::None;
                if r > 12.0 {
                    painter.circle_filled(p, 1.0, Color32::WHITE);
                }
            }
        }
        ctx.set_cursor_icon(icon);
    }
}

struct Input {
    pos: Option<Pos2>,
    moves: Vec<Pos2>,
    delta: Vec2,
    pressed: bool,
    down: bool,
    mid_pressed: bool,
    mid_down: bool,
    mods: Modifiers,
    space: bool,
    scroll: Vec2,
    zoom: f32,
    rot: f32,
    time: f64,
}

fn ipos(p: Pos2) -> (i32, i32) {
    (p.x.floor() as i32, p.y.floor() as i32)
}

fn pick_color(app: &mut App, p: Pos2) {
    let (x, y) = ipos(p);
    if let Some(c) = composite::sample(&app.doc.state, x, y) {
        if c[3] > 0 {
            app.settings.fg = [c[0], c[1], c[2], 255];
        }
    }
}

fn quick_step(app: &mut App, p: Pos2) {
    let Drag::Quick { src, mode, .. } = &app.drag else { return };
    let r = app.settings.quick_size * 0.5;
    let reach = (r * 3.0).max(12.0);
    let bound = IRect::enclosing(p.x - reach, p.y - reach, p.x + reach, p.y + reach);
    let mut m = selection::flood_mask(src, ipos(p), app.settings.tolerance, true, Some(bound));
    // Always take what's directly under the brush too.
    let dab = selection::ellipse_mask(m.w, m.h, p.x - r, p.y - r, p.x + r, p.y + r);
    m.data.iter_mut().zip(&dab.data).for_each(|(a, b)| *a = (*a).max(*b));
    // Replace and Intersect start from an empty selection and grow the stroke.
    let mode = if *mode == Combine::Subtract { Combine::Subtract } else { Combine::Add };
    let sel = selection::combine(app.doc.state.selection.as_deref(), &m, mode);
    app.doc.state.selection = sel.map(Arc::new);
}

fn press(app: &mut App, p: Pos2, mods: Modifiers) {
    let tool = app.tool;
    let pt = (p.x, p.y);
    match tool {
        Tool::Hand => {}
        Tool::Zoom => app.drag = Drag::ZoomScrub,
        Tool::Eyedropper => {
            pick_color(app, p);
            app.drag = Drag::Pick;
        }
        Tool::Move => {
            let (x, y) = ipos(p);
            if app.settings.auto_select {
                if let Some(id) = app.doc.layer_at(x, y) {
                    app.doc.state.active = id;
                }
            }
            if let Some(l) = app.doc.state.active_layer() {
                if l.locked {
                    return app.toast("Layer is locked");
                }
                app.drag = Drag::Move { before: app.doc.begin(), start: p, orig: (l.x, l.y), moved: false };
            }
        }
        Tool::Brush | Tool::Pencil | Tool::Eraser | Tool::Smudge | Tool::Clone => {
            if mods.alt {
                if tool == Tool::Clone {
                    app.clone_src = Some(p);
                    app.clone_off = None;
                } else if tool != Tool::Smudge {
                    pick_color(app, p);
                    app.drag = Drag::Pick;
                }
                return;
            }
            let kind = match tool {
                Tool::Brush => PaintKind::Brush(app.settings.fg),
                Tool::Pencil => PaintKind::Pencil(app.settings.fg),
                Tool::Eraser => PaintKind::Eraser,
                Tool::Smudge => PaintKind::Smudge { strength: app.settings.smudge_strength },
                _ => {
                    let Some(src) = app.clone_src else {
                        return app.toast("Option-click to choose what to clone from");
                    };
                    let (dx, dy) = *app
                        .clone_off
                        .get_or_insert(((src.x - p.x).round() as i32, (src.y - p.y).round() as i32));
                    PaintKind::Clone { dx, dy }
                }
            };
            let params = *app.settings.params_mut(tool).unwrap();
            match Stroke::begin(&mut app.doc, kind, params, pt) {
                Some(s) => app.drag = Drag::Stroke(s),
                None => app.toast("This layer is hidden or locked"),
            }
        }
        Tool::Gradient => match GradientOp::begin(&mut app.doc) {
            Some(op) => app.drag = Drag::Gradient { op, start: p, end: p },
            None => app.toast("This layer is hidden or locked"),
        },
        Tool::Bucket => {
            let s = &app.settings;
            let (fg, tol, contig, all, op) = (s.fg, s.tolerance, s.contiguous, s.sample_all_layers, s.fill_opacity);
            if app.doc.canvas().contains(ipos(p).0, ipos(p).1) {
                fill::bucket(&mut app.doc, ipos(p), fg, tol, contig, all, op);
            }
        }
        Tool::RectSelect | Tool::EllipseSelect => {
            app.drag = Drag::Marquee { start: p, cur: p, mode: combine_mode(mods, app.settings.sel_mode), ellipse: tool == Tool::EllipseSelect };
        }
        Tool::Lasso => app.drag = Drag::Lasso { pts: vec![p], mode: combine_mode(mods, app.settings.sel_mode) },
        Tool::MagicWand => {
            let s = &app.settings;
            let seed = ipos(p);
            if !app.doc.canvas().contains(seed.0, seed.1) {
                return app.doc.deselect();
            }
            if let Some(src) = fill::sample_source(&app.doc.state, s.sample_all_layers) {
                let m = selection::flood_mask(&src, seed, s.tolerance, s.contiguous, None);
                app.doc.select("Magic Wand", &m, combine_mode(mods, app.settings.sel_mode));
            }
        }
        Tool::QuickSelect => {
            let Some(src) = fill::sample_source(&app.doc.state, app.settings.sample_all_layers) else { return };
            let before = app.doc.begin();
            let mode = combine_mode(mods, app.settings.sel_mode);
            if matches!(mode, Combine::Replace | Combine::Intersect) {
                app.doc.state.selection = None;
            }
            app.drag = Drag::Quick { before, src, mode, last: p };
            quick_step(app, p);
        }
    }
}

fn dragged(app: &mut App, p: Pos2, screen: Pos2, inp: &Input) {
    let tool_fill = app.settings.gradient_fill;
    match &mut app.drag {
        Drag::None => {}
        Drag::Pan => {
            // Applied once per frame below, not per pointer sample.
            if inp.moves.last().is_none_or(|l| *l == screen) {
                app.view.pan(inp.delta);
            }
        }
        Drag::ZoomScrub => {
            if inp.moves.last().is_none_or(|l| *l == screen) && inp.delta.x != 0.0 {
                app.view.zoom_about(screen, (inp.delta.x * 0.01).exp());
                app.zoom_scrubbed = true;
            }
        }
        Drag::Pick => pick_color(app, p),
        Drag::Stroke(s) => s.line_to(&mut app.doc, (p.x, p.y)),
        Drag::Gradient { op, start, end } => {
            *end = p;
            let s = &app.settings;
            let to = match tool_fill {
                GradientFill::ForegroundToBackground => s.bg,
                GradientFill::ForegroundToTransparent => [s.fg[0], s.fg[1], s.fg[2], 0],
            };
            let gp = GradientParams { shape: s.gradient_shape, from: s.fg, to, opacity: s.gradient_opacity };
            if (*end - *start).length() >= 1.0 {
                op.update(&mut app.doc, (start.x, start.y), (p.x, p.y), &gp);
            }
        }
        Drag::Move { start, orig, moved, .. } => {
            let d = p - *start;
            let (nx, ny) = (orig.0 + d.x.round() as i32, orig.1 + d.y.round() as i32);
            if let Some(l) = app.doc.state.active_layer_mut() {
                if (l.x, l.y) != (nx, ny) {
                    let old = l.rect();
                    (l.x, l.y) = (nx, ny);
                    let new = l.rect();
                    *moved = true;
                    app.doc.mark_dirty(old);
                    app.doc.mark_dirty(new);
                }
            }
        }
        Drag::Marquee { cur, .. } => *cur = p,
        Drag::Lasso { pts, .. } => {
            if pts.last().is_none_or(|l| (*l - p).length() * app.view.scale() >= 1.5) {
                pts.push(p);
            }
        }
        Drag::Quick { last, .. } => {
            if (*last - p).length() >= (app.settings.quick_size * 0.25).max(1.0) {
                *last = p;
                quick_step(app, p);
            }
        }
    }
}

fn release(app: &mut App) {
    let (w, h) = (app.doc.state.width, app.doc.state.height);
    match std::mem::replace(&mut app.drag, Drag::None) {
        Drag::None | Drag::Pan | Drag::Pick => {}
        Drag::ZoomScrub => {
            // A plain click zooms in a step (out with Option/Alt).
            if !std::mem::take(&mut app.zoom_scrubbed) {
                let (pos, alt) = app.ctx_pointer();
                app.view.zoom_about(pos, if alt { 0.5 } else { 2.0 });
            }
        }
        Drag::Stroke(s) => s.finish(&mut app.doc),
        Drag::Gradient { op, start, end } => {
            if (end - start).length() < 1.0 {
                op.cancel(&mut app.doc);
            } else {
                op.finish(&mut app.doc);
            }
        }
        Drag::Move { before, moved, .. } => {
            if moved {
                app.doc.commit("Move Layer", before);
            }
        }
        Drag::Marquee { start, cur, mode, ellipse } => {
            let r = Rect::from_two_pos(start, cur);
            if r.width() < 1.0 || r.height() < 1.0 {
                if mode == Combine::Replace {
                    app.doc.deselect();
                }
                return;
            }
            let (m, name) = if ellipse {
                (selection::ellipse_mask(w, h, r.min.x, r.min.y, r.max.x, r.max.y), "Elliptical Selection")
            } else {
                let ir = IRect::new(r.min.x.round() as i32, r.min.y.round() as i32, r.max.x.round() as i32, r.max.y.round() as i32);
                (selection::rect_mask(w, h, ir), "Rectangular Selection")
            };
            app.doc.select(name, &m, mode);
        }
        Drag::Lasso { pts, mode } => {
            if pts.len() < 3 {
                if mode == Combine::Replace {
                    app.doc.deselect();
                }
                return;
            }
            let pts: Vec<(f32, f32)> = pts.iter().map(|p| (p.x, p.y)).collect();
            app.doc.select("Free Selection", &selection::polygon_mask(w, h, &pts), mode);
        }
        Drag::Quick { before, mode, .. } => {
            if mode == Combine::Intersect {
                let sel = app.doc.state.selection.take().and_then(|m| selection::combine(before.selection.as_deref(), &m, mode));
                app.doc.state.selection = sel.map(Arc::new);
            }
            app.doc.commit_quiet("Quick Selection", before)
        }
    }
}

/// Abort the current drag, restoring the document where needed.
pub fn cancel(app: &mut App) {
    match std::mem::replace(&mut app.drag, Drag::None) {
        Drag::Gradient { op, .. } => op.cancel(&mut app.doc),
        Drag::Move { before, .. } | Drag::Quick { before, .. } => {
            app.doc.state = before;
            app.doc.mark_all_dirty();
        }
        Drag::Stroke(s) => {
            s.finish(&mut app.doc);
            app.doc.undo();
        }
        _ => {}
    }
}

fn draw_ants(painter: &egui::Painter, view: &View, segs: &[[i32; 4]], time: f64) {
    let vis = view.visible_doc_rect().expand(1.0);
    let scale = view.scale();
    let dash = 5.0 / scale;
    let phase = (time * 12.0) as f32 / scale;
    let hw = 0.5;
    let mut mesh = Mesh::default();
    let mut quad = |a: Pos2, b: Pos2, color: Color32| {
        let (a, b) = (view.to_screen(a), view.to_screen(b));
        let d = b - a;
        let len = d.length();
        if len <= 0.0 {
            return;
        }
        let u = d / len;
        let n = Vec2::new(-u.y, u.x) * hw;
        let (a, b) = (a - u * hw, b + u * hw);
        let i = mesh.vertices.len() as u32;
        for p in [a + n, b + n, b - n, a - n] {
            mesh.colored_vertex(p, color);
        }
        mesh.indices.extend_from_slice(&[i, i + 1, i + 2, i, i + 2, i + 3]);
    };
    for s in segs {
        let horiz = s[1] == s[3];
        let (fixed, mut t0, mut t1) = if horiz { (s[1], s[0], s[2]) } else { (s[0], s[1], s[3]) };
        let (fmin, fmax, tmin, tmax) =
            if horiz { (vis.min.y, vis.max.y, vis.min.x, vis.max.x) } else { (vis.min.x, vis.max.x, vis.min.y, vis.max.y) };
        if (fixed as f32) < fmin || fixed as f32 > fmax {
            continue;
        }
        t0 = t0.max(tmin.floor() as i32);
        t1 = t1.min(tmax.ceil() as i32);
        if t0 >= t1 {
            continue;
        }
        // Split into alternating black / white dashes that slide with `phase`.
        let mut t = t0 as f32;
        let end = t1 as f32;
        while t < end {
            let k = ((t + fixed as f32 + phase) / dash).floor();
            let next = ((k + 1.0) * dash - fixed as f32 - phase).max(t + dash * 0.01).min(end);
            let color = if k as i64 % 2 == 0 { Color32::BLACK } else { Color32::WHITE };
            let f = fixed as f32;
            if horiz { quad(pos2(t, f), pos2(next, f), color) } else { quad(pos2(f, t), pos2(f, next), color) }
            t = next;
        }
    }
    painter.add(Shape::mesh(mesh));
}
