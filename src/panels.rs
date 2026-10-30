//! What each panel shows. Panels only read the app and queue commands or
//! make small edits; any panel can be placed anywhere, or twice.

use egui::{Align, Align2, Color32, CursorIcon, FontId, Id, Layout, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Ui, UiBuilder, pos2, vec2};

use crate::adapters::Sections;
use crate::layout::Part;
use crate::project::{Clip, Track, db};
use crate::theme::{Palette, REC};
use crate::{App, keys, log};

/// What dragging on the Tracks panel is doing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drag {
    None,
    Scrub,
    /// Trimming a clip's start (`left`) or end; the clip as it was when the
    /// drag began, and the pointer's x then.
    Trim { track: usize, clip: usize, left: bool, at: f64, src_in: f64, src_out: f64, x0: f32 },
    /// Moving a clip; `grab` is where on it the pointer took hold, in
    /// seconds from its start.
    Move { track: usize, clip: usize, grab: f64 },
}

/// A media file dragged from the Media panel.
struct MediaDrag(usize);

/// The width of the track headers on the Tracks panel.
const HEAD: f32 = 168.0;
const RULER: f32 = 22.0;

pub fn draw(app: &mut App, kind: &str, part: Part, ui: &mut Ui) {
    if part == Part::Header {
        header(app, kind, ui);
        return;
    }
    // The tracks and mixer run edge to edge; the rest get a margin.
    let edge = matches!(kind, "tracks" | "mixer");
    let rect = if edge { ui.max_rect() } else { ui.max_rect().shrink2(vec2(12.0, 8.0)) };
    let mut ui = ui.new_child(UiBuilder::new().max_rect(rect));
    let ui = &mut ui;
    match kind {
        "tracks" => tracks(app, ui),
        "mixer" => mixer(app, ui),
        "media" => media(app, ui),
        "transport" => transport(app, ui),
        "clip" => clip(app, ui),
        "export" => export(app, ui),
        "log" => log_panel(ui),
        "help" => help(app, ui),
        "adapters" => adapters_panel(app, ui),
        k if k.starts_with("adapter:") => adapter_panel(app, k, ui),
        _ => {}
    }
}

/// Small controls at the right end of a panel's header.
fn header(app: &mut App, kind: &str, ui: &mut Ui) {
    let small = |ui: &mut Ui, text: &str, tip: &str| {
        ui.add(egui::Button::new(RichText::new(text).size(11.0)).frame(false).min_size(vec2(18.0, 18.0))).on_hover_text(tip)
    };
    match kind {
        "media" => {
            if small(ui, "+ Import", "Add sound files").clicked() {
                app.queue.push("import".into());
            }
        }
        "adapters" => {
            if small(ui, "+ Install", "Install an adapter from its zip (or drop it on the window)").clicked() {
                app.queue.push("install adapter".into());
            }
        }
        "tracks" => {
            for (text, cmd, tip) in [("Fit", "zoom fit", "Fit the project"), ("+", "zoom in", "Zoom in"), ("−", "zoom out", "Zoom out"), ("+ Track", "track new", "Add a track")] {
                if small(ui, text, tip).clicked() {
                    app.queue.push(cmd.into());
                }
            }
        }
        "mixer" => {
            ui.label(RichText::new(format!("{} kHz", f64::from(app.engine.rate) / 1000.0)).size(10.5).color(app.palette.weak));
        }
        _ => {}
    }
}

/// "1:02.345" (m:ss.mmm), with hours when there are some.
pub fn timecode(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as i64;
    let (h, m, s, ms) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if h > 0 { format!("{h}:{m:02}:{s:02}.{ms:03}") } else { format!("{m}:{s:02}.{ms:03}") }
}

fn short_time(t: f64) -> String {
    let s = t.round() as i64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// A fader's position, 0 to 1, for a level in dB: +6 dB at the top, 0 dB
/// at 71%, -6 at half way, -60 near the bottom. Meters use the same scale, so a
/// meter and its fader line up.
pub fn fader_pos(db: f32) -> f32 {
    if db <= -90.0 { 0.0 } else { 10f32.powf((db - 6.0) / 40.0).clamp(0.0, 1.0) }
}

pub fn fader_db(pos: f32) -> f32 {
    if pos <= 0.003 { -90.0 } else { (6.0 + 40.0 * pos.min(1.0).log10()).max(-90.0) }
}

fn db_text(d: f32) -> String {
    if d <= -90.0 { "−∞".into() } else { format!("{:+.1}", d).replace('-', "−") }
}

fn tracks(app: &mut App, ui: &mut Ui) {
    let rect = ui.max_rect();
    let pal = app.palette;
    let accent = pal.accent;
    let lanes = Rect::from_min_max(pos2(rect.left() + HEAD, rect.top()), rect.max);
    app.view.width = lanes.width();
    if app.view.fit {
        app.zoom_fit();
    }
    let ruler = Rect::from_min_size(lanes.min, vec2(lanes.width(), RULER));
    let body = Rect::from_min_max(pos2(rect.left(), ruler.bottom()), rect.max);
    let th = app.settings.look.track_height.clamp(32.0, 400.0);
    let n = app.project.tracks.len();
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, pal.panel);
    painter.rect_filled(Rect::from_min_max(rect.min, pos2(rect.left() + HEAD, ruler.bottom())), 0.0, pal.header);
    painter.rect_filled(ruler, 0.0, pal.header);
    painter.rect_filled(Rect::from_x_y_ranges(rect.x_range(), ruler.bottom()..=ruler.bottom() + 1.0), 0.0, pal.line);
    painter.rect_filled(Rect::from_x_y_ranges(lanes.left() - 1.0..=lanes.left(), rect.y_range()), 0.0, pal.line);

    let bg = ui.interact(lanes, ui.scope_id().with("lanes"), Sense::click_and_drag());
    let pointer = ui.input(|i| i.pointer.interact_pos());

    // Scroll sideways, zoom (cmd/ctrl + scroll, or pinch) under the
    // pointer, and scroll down through tracks with shift or over headers.
    let over = ui.rect_contains_pointer(rect);
    if over {
        let (scroll, zoom, ptr, shift) = ui.input(|i| (i.smooth_scroll_delta(), i.zoom_delta(), i.pointer.hover_pos(), i.modifiers.shift));
        let on_heads = ptr.is_some_and(|p| p.x < lanes.left());
        if zoom != 1.0 {
            let x = ptr.map_or(0.0, |p| p.x - lanes.left());
            let at = app.view.scroll + (x / app.view.pps) as f64;
            app.view.pps = (app.view.pps * zoom).clamp(0.5, 4000.0);
            app.view.scroll = at - (x / app.view.pps) as f64;
        } else if on_heads || shift {
            app.view.down -= scroll.y + scroll.x;
        } else if scroll != egui::Vec2::ZERO {
            app.view.scroll -= (scroll.x / app.view.pps) as f64;
            app.view.down -= scroll.y;
        }
    }
    let total_h = (n as f32 + 1.0) * th;
    app.view.down = app.view.down.clamp(0.0, (total_h - body.height()).max(0.0));
    let duration = app.duration();
    let max_scroll = (duration - (lanes.width() * 0.5 / app.view.pps) as f64).max(0.0);
    let playhead = app.playhead();
    // While playing, the view follows the playhead.
    if app.engine.playing() {
        let width = (lanes.width() / app.view.pps) as f64;
        if playhead > app.view.scroll + 0.95 * width || playhead < app.view.scroll {
            app.view.scroll = playhead - 0.1 * width;
        }
    }
    app.view.scroll = app.view.scroll.clamp(0.0, max_scroll.max(playhead));

    let (pps, scroll, down) = (app.view.pps, app.view.scroll, app.view.down);
    let x_of = |t: f64| lanes.left() + ((t - scroll) as f32) * pps;
    let t_of = |x: f32| scroll + ((x - lanes.left()) / pps) as f64;
    let y_of = |i: usize| body.top() + i as f32 * th - down;
    let track_at = |y: f32| ((y - body.top() + down) / th).floor().max(0.0) as usize;
    let t_right = t_of(lanes.right());

    // Ruler: a tick step that leaves room for its labels.
    let steps = [0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0];
    let step = steps.iter().copied().find(|s| s * pps as f64 >= 70.0).unwrap_or(3600.0);
    let mut t = (scroll / step).floor() * step;
    while t <= t_right {
        let x = x_of(t);
        painter.line_segment([pos2(x, ruler.bottom() - 5.0), pos2(x, ruler.bottom())], Stroke::new(1.0, pal.weak));
        let label = if step < 1.0 { timecode(t) } else { short_time(t) };
        painter.text(pos2(x + 4.0, ruler.top() + 4.0), Align2::LEFT_TOP, label, FontId::monospace(9.5), pal.weak);
        t += step;
    }
    painter.text(pos2(rect.left() + 10.0, ruler.center().y), Align2::LEFT_CENTER, format!("{n} track{}", if n == 1 { "" } else { "s" }), FontId::proportional(11.0), pal.weak);

    let lane_painter = painter.with_clip_rect(Rect::from_min_max(pos2(lanes.left(), body.top()), lanes.max).intersect(ui.clip_rect()));
    let head_painter = painter.with_clip_rect(Rect::from_min_max(pos2(rect.left(), body.top()), pos2(lanes.left() - 1.0, rect.bottom())).intersect(ui.clip_rect()));
    let mut new_drag = None;
    let rate = f64::from(app.engine.rate);
    let first = (down / th).floor() as usize;
    let last = (((down + body.height()) / th).ceil() as usize).min(n);
    for ti in first..last {
        let y = y_of(ti);
        let row = Rect::from_x_y_ranges(lanes.x_range(), y..=y + th);
        let selected_track = app.selected_track == Some(ti);
        if selected_track {
            lane_painter.rect_filled(row, 0.0, pal.text.gamma_multiply(0.03));
        }
        lane_painter.rect_filled(Rect::from_x_y_ranges(rect.x_range(), row.bottom() - 1.0..=row.bottom()), 0.0, pal.line);
        head_painter.rect_filled(Rect::from_x_y_ranges(rect.x_range(), row.bottom() - 1.0..=row.bottom()), 0.0, pal.line);

        // The header: name, mute, solo, arm, and the level.
        let head = Rect::from_min_max(pos2(rect.left(), y), pos2(lanes.left() - 1.0, y + th - 1.0));
        track_head(app, ui, ti, head, body);

        // Clips, from the first one reaching into view.
        let track = &app.project.tracks[ti];
        let color = pal.clip(ti);
        let from = track.first_reaching(scroll);
        let clips: Vec<(usize, Clip)> = (from..track.clips.len()).take_while(|&i| track.clips[i].at < t_right).map(|i| (i, track.clips[i].clone())).collect();
        for (ci, c) in clips {
            let r = Rect::from_x_y_ranges(x_of(c.at)..=x_of(c.end()), row.top() + 2.0..=row.bottom() - 3.0);
            let shape = Rect::from_x_y_ranges(r.left() + 0.5..=r.right() - 0.5, r.y_range());
            let moving = matches!(app.view.drag, Drag::Move { track, clip, .. } if track == ti && clip == ci);
            lane_painter.rect_filled(shape, 3.0, if moving { color.gamma_multiply(0.4) } else { color });
            if let Some(sound) = app.sound(c.media) {
                let src_at = |x: f32| c.src_in + t_of(x) - c.at;
                let frames = |t: f64| (t * rate).max(0.0) as usize;
                waveform(&lane_painter, shape, |x| sound.peaks.max(frames(src_at(x)), frames(src_at(x + 1.0)).max(frames(src_at(x)) + 1)), pal.text.gamma_multiply(0.45));
            } else {
                lane_painter.text(shape.center(), Align2::CENTER_CENTER, "reading…", FontId::proportional(10.5), pal.weak);
            }
            if app.selected_clip == Some((ti, ci)) {
                lane_painter.rect_stroke(shape, 3.0, Stroke::new(1.5, accent), StrokeKind::Inside);
            }
            if r.width() > 30.0
                && let Some(m) = app.project.media.get(c.media)
            {
                let p = lane_painter.with_clip_rect(r.shrink(3.0).intersect(lane_painter.clip_rect()));
                let name = p.layout_no_wrap(m.name(), FontId::proportional(10.5), pal.text);
                let back = Rect::from_min_size(r.left_top() + vec2(4.0, 3.0), name.size() + vec2(6.0, 2.0));
                p.rect_filled(back, 2.0, color.gamma_multiply(0.85));
                p.galley(back.min + vec2(3.0, 1.0), name, pal.text);
            }
            if !lanes.intersects(r) || r.bottom() < body.top() {
                continue;
            }
            let hit = r.intersect(Rect::from_min_max(pos2(lanes.left(), body.top()), lanes.max));
            let resp = ui.interact(hit, ui.scope_id().with(("clip", ti, ci)), Sense::click_and_drag());
            if resp.clicked() || resp.drag_started() {
                app.selected_clip = Some((ti, ci));
                app.selected_track = Some(ti);
            }
            if resp.clicked()
                && let Some(p) = pointer
            {
                app.seek(t_of(p.x));
            }
            if resp.drag_started()
                && let Some(p) = pointer
            {
                new_drag = Some(Drag::Move { track: ti, clip: ci, grab: t_of(p.x) - c.at });
            }
            // Trim handles at each end.
            for left in [true, false] {
                let x = if left { r.left() } else { r.right() };
                let h = Rect::from_x_y_ranges(x - 4.0..=x + 4.0, hit.y_range()).intersect(hit.expand2(vec2(4.0, 0.0)));
                let hr = ui.interact(h, ui.scope_id().with(("trim", ti, ci, left)), Sense::drag());
                if hr.hovered() || hr.dragged() {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    lane_painter.rect_filled(Rect::from_x_y_ranges(x - 1.5..=x + 1.5, r.y_range()), 1.0, accent);
                }
                if hr.drag_started()
                    && let Some(p) = pointer
                {
                    app.snapshot();
                    app.selected_clip = Some((ti, ci));
                    app.selected_track = Some(ti);
                    new_drag = Some(Drag::Trim { track: ti, clip: ci, left, at: c.at, src_in: c.src_in, src_out: c.src_out, x0: p.x });
                }
            }
        }

        // The take being recorded onto this track.
        if let Some(rec) = app.recording.as_ref().filter(|r| r.track == ti) {
            let end = rec.at + rec.seconds();
            let r = Rect::from_x_y_ranges(x_of(rec.at)..=x_of(end).max(x_of(rec.at) + 2.0), row.top() + 2.0..=row.bottom() - 3.0);
            lane_painter.rect_filled(r, 3.0, REC.gamma_multiply(0.35));
            let peaks = rec.live.peaks.lock().unwrap_or_else(|e| e.into_inner());
            let per = rec.rate as f64 / crate::media::BLOCK as f64;
            waveform(&lane_painter, r, |x| {
                let (a, b) = (((t_of(x) - rec.at) * per).max(0.0) as usize, ((t_of(x + 1.0) - rec.at) * per).max(0.0) as usize);
                let b = b.max(a + 1).min(peaks.len());
                if a < b { f32::from(peaks[a..b].iter().copied().max().unwrap_or(0)) / 255.0 } else { 0.0 }
            }, pal.text.gamma_multiply(0.7));
            lane_painter.rect_stroke(r, 3.0, Stroke::new(1.5, REC), StrokeKind::Inside);
        }
    }
    if let Some(d) = new_drag {
        app.view.drag = d;
    }
    if n == 0 {
        painter.text(body.center(), Align2::CENTER_CENTER, "Drop sound files anywhere, or press R to record", FontId::proportional(14.0), pal.weak);
    }

    // Ongoing drags.
    let released = ui.input(|i| i.pointer.any_released());
    let alt = ui.input(|i| i.modifiers.alt);
    let near = (8.0 / pps) as f64;
    match app.view.drag {
        Drag::Trim { track, clip, left, at, src_in, src_out, x0 } if app.project.tracks.get(track).is_some_and(|t| clip < t.clips.len()) => {
            if let Some(p) = pointer {
                let min = 0.01;
                let len = app.project.media.get(app.project.tracks[track].clips[clip].media).map_or(f64::MAX, |m| m.duration);
                let mut d = ((p.x - x0) / pps) as f64;
                let edge = if left { at + d } else { at + (src_out - src_in) + d };
                if !alt && let Some(s) = snap_to(app, edge, near, Some((track, clip))) {
                    d += s - edge;
                }
                let c = &mut app.project.tracks[track].clips[clip];
                if left {
                    let d = d.clamp(-src_in.min(at), src_out - src_in - min);
                    c.at = at + d;
                    c.src_in = src_in + d;
                } else {
                    c.src_out = (src_out + d).clamp(src_in + min, len);
                }
                app.edited();
            }
            if released {
                let t = &mut app.project.tracks[track];
                let i = t.sort(clip);
                app.selected_clip = Some((track, i));
            }
        }
        Drag::Move { track, clip, grab } if app.project.tracks.get(track).is_some_and(|t| clip < t.clips.len()) => {
            if let Some(p) = pointer {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
                let c = app.project.tracks[track].clips[clip].clone();
                let mut at = (t_of(p.x) - grab).max(0.0);
                if !alt {
                    if let Some(s) = snap_to(app, at, near, Some((track, clip))) {
                        at = s;
                    } else if let Some(s) = snap_to(app, at + c.len(), near, Some((track, clip))) {
                        at = (s - c.len()).max(0.0);
                    }
                }
                let to = track_at(p.y).min(n);
                let y = y_of(to);
                let ghost = Rect::from_x_y_ranges(x_of(at)..=x_of(at + c.len()), y + 2.0..=y + th - 3.0);
                lane_painter.rect_filled(ghost, 3.0, pal.clip(to).gamma_multiply(0.8));
                lane_painter.rect_stroke(ghost, 3.0, Stroke::new(1.5, accent), StrokeKind::Inside);
                if released && (to != track || (at - c.at).abs() > 1e-9) {
                    app.snapshot();
                    let c = app.project.tracks[track].clips.remove(clip);
                    if to >= app.project.tracks.len() {
                        let k = app.project.tracks.len() + 1;
                        app.project.tracks.push(Track::new(format!("Track {k}")));
                    }
                    let i = app.project.tracks[to].insert(Clip { at, ..c });
                    app.selected_clip = Some((to, i));
                    app.selected_track = Some(to);
                }
            }
        }
        Drag::Scrub => {
            if let Some(p) = pointer {
                app.seek(t_of(p.x));
            }
        }
        _ => {}
    }
    if bg.drag_started() && app.view.drag == Drag::None {
        app.view.drag = Drag::Scrub;
    }
    if bg.clicked()
        && let Some(p) = pointer
    {
        app.seek(t_of(p.x));
        app.selected_clip = None;
        let t = track_at(p.y);
        app.selected_track = (p.y > body.top() && t < n).then_some(t);
    }
    if released || !ui.input(|i| i.pointer.any_down()) {
        app.view.drag = Drag::None;
    }

    // Media dragged in from the Media panel: onto a track at the pointer,
    // or below the last to make a new one.
    if let Some(p) = pointer
        && bg.contains_pointer()
        && let Some(d) = bg.dnd_hover_payload::<MediaDrag>()
        && let Some(m) = app.project.media.get(d.0)
    {
        let to = track_at(p.y).min(n);
        let y = y_of(to);
        let ghost = Rect::from_x_y_ranges(p.x..=x_of(t_of(p.x) + m.duration), y + 2.0..=y + th - 3.0);
        lane_painter.rect_stroke(ghost, 3.0, Stroke::new(1.5, accent), StrokeKind::Inside);
    }
    if let Some(d) = bg.dnd_release_payload::<MediaDrag>()
        && let Some(p) = pointer
    {
        let to = track_at(p.y);
        app.place(d.0, (to < n).then_some(to), t_of(p.x));
    }

    // Playhead.
    let x = x_of(playhead);
    if lanes.x_range().contains(x) {
        let color = if app.recording.is_some() { REC } else { accent };
        painter.line_segment([pos2(x, ruler.top()), pos2(x, rect.bottom())], Stroke::new(1.5, color));
        painter.add(egui::Shape::convex_polygon(vec![pos2(x - 5.0, ruler.top()), pos2(x + 5.0, ruler.top()), pos2(x, ruler.top() + 7.0)], color, Stroke::NONE));
    }
}

/// Where `t` snaps to: the playhead, the start, or another clip's edge,
/// within `near` seconds. `skip` is the clip being dragged.
fn snap_to(app: &App, t: f64, near: f64, skip: Option<(usize, usize)>) -> Option<f64> {
    let mut best: Option<f64> = None;
    let mut consider = |s: f64| {
        if (s - t).abs() <= near && best.is_none_or(|b| (s - t).abs() < (b - t).abs()) {
            best = Some(s);
        }
    };
    consider(0.0);
    consider(app.playhead());
    for (ti, track) in app.project.tracks.iter().enumerate() {
        for (ci, c) in track.clips.iter().enumerate() {
            if Some((ti, ci)) != skip {
                consider(c.at);
                consider(c.end());
            }
        }
    }
    best
}

/// A waveform inside `shape`: `peak(x)` is the loudest moment, 0 to 1,
/// under the point at `x`. One bar a point, all in one mesh.
fn waveform(p: &egui::Painter, shape: Rect, peak: impl Fn(f32) -> f32, color: Color32) {
    let view = p.clip_rect().intersect(shape);
    if view.width() <= 0.0 {
        return;
    }
    let lane = shape.shrink2(vec2(0.0, 4.0));
    let (mid, half) = (lane.center().y, lane.height() * 0.5);
    let mut mesh = egui::Mesh::default();
    let mut x = view.left().floor();
    while x < view.right() {
        let h = (peak(x) * half).max(0.5);
        mesh.add_colored_rect(Rect::from_x_y_ranges(x..=x + 1.0, mid - h..=mid + h), color);
        x += 1.0;
    }
    p.add(mesh);
}

/// A track's header on the Tracks panel.
fn track_head(app: &mut App, ui: &mut Ui, i: usize, head: Rect, body: Rect) {
    let pal = app.palette;
    let visible = head.intersect(Rect::from_min_max(pos2(head.left(), body.top()), body.max));
    if visible.height() < 4.0 {
        return;
    }
    let click = ui.interact(visible, ui.scope_id().with(("head", i)), Sense::click());
    if click.clicked() {
        app.selected_track = Some(i);
    }
    let selected = app.selected_track == Some(i);
    if selected {
        ui.painter().with_clip_rect(visible).rect_filled(Rect::from_min_size(head.min, vec2(2.0, head.height())), 0.0, pal.accent);
    }
    let mut hui = ui.new_child(UiBuilder::new().max_rect(head.shrink2(vec2(8.0, 6.0))));
    hui.set_clip_rect(visible);
    hui.spacing_mut().item_spacing = vec2(4.0, 4.0);
    let mut changed = false;
    let track = &mut app.project.tracks[i];
    let name = egui::TextEdit::singleline(&mut track.name).frame(egui::Frame::NONE).font(FontId::proportional(12.5)).desired_width(head.width() - 16.0);
    if hui.add(name).changed() {
        app.session_dirty = true;
        app.dirty = true;
    }
    hui.horizontal(|ui| {
        changed |= toggle(ui, &mut track.mute, "M", "Mute (m)", pal.weak, &pal).clicked();
        changed |= toggle(ui, &mut track.solo, "S", "Solo (shift+S)", Color32::from_rgb(0xf2, 0xc1, 0x4e), &pal).clicked();
        let mut armed = track.armed;
        if toggle(ui, &mut armed, "R", "Record onto this track (shift+R)", REC, &pal).clicked() {
            app.queue.push("track arm".into());
            app.selected_track = Some(i);
        }
        ui.label(RichText::new(db_text(track.gain_db)).monospace().size(10.5).color(pal.weak));
    });
    if changed {
        app.mix_changed();
    }
    // A thin meter along the header's bottom.
    let level = app.levels.get(i).copied().unwrap_or(0.0);
    if level > 0.0 {
        let w = fader_pos(db(level)) * (head.width() - 16.0);
        let bar = Rect::from_min_size(pos2(head.left() + 8.0, head.bottom() - 4.0), vec2(w, 2.0));
        ui.painter().with_clip_rect(visible).rect_filled(bar, 1.0, meter_color(level, &pal));
    }
}

/// A small square button that's lit while `on`.
fn toggle(ui: &mut Ui, on: &mut bool, text: &str, tip: &str, lit: Color32, pal: &Palette) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(20.0, 18.0), Sense::click());
    let p = ui.painter();
    if *on {
        p.rect_filled(rect, 3.0, lit);
    } else {
        p.rect_filled(rect, 3.0, if r.hovered() { ui.visuals().widgets.hovered.weak_bg_fill } else { ui.visuals().widgets.inactive.weak_bg_fill });
    }
    let ink = if *on { Color32::from_rgb(0x12, 0x13, 0x15) } else { pal.weak };
    p.text(rect.center(), Align2::CENTER_CENTER, text, FontId::proportional(10.5), ink);
    if r.clicked() {
        *on = !*on;
    }
    r.on_hover_text(tip)
}

fn meter_color(level: f32, pal: &Palette) -> Color32 {
    if level >= 1.0 {
        REC
    } else if level >= 0.5 {
        Color32::from_rgb(0xf2, 0xc1, 0x4e)
    } else {
        pal.accent
    }
}

/// One strip a track, then the master: meter and fader, pan, mute, solo.
fn mixer(app: &mut App, ui: &mut Ui) {
    let rect = ui.max_rect();
    let pal = app.palette;
    ui.painter().rect_filled(rect, 0.0, pal.panel);
    const W: f32 = 78.0;
    let n = app.project.tracks.len();
    let master = Rect::from_min_max(pos2(rect.right() - W - 8.0, rect.top()), rect.max);
    ui.painter().rect_filled(Rect::from_x_y_ranges(master.left() - 1.0..=master.left(), rect.y_range()), 0.0, pal.line);
    let strips = Rect::from_min_max(rect.min, pos2(master.left() - 1.0, rect.bottom()));
    let mut sui = ui.new_child(UiBuilder::new().max_rect(strips));
    egui::ScrollArea::horizontal().auto_shrink(false).show(&mut sui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for i in 0..n {
                let (r, _) = ui.allocate_exact_size(vec2(W, strips.height()), Sense::hover());
                strip(app, ui, Some(i), r);
                ui.painter().rect_filled(Rect::from_x_y_ranges(r.right() - 1.0..=r.right(), r.y_range()), 0.0, pal.line);
            }
            if n == 0 {
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.add_space(10.0);
                    ui.weak("Each track gets a strip here.");
                });
            }
        });
    });
    strip(app, ui, None, master.shrink2(vec2(4.0, 0.0)));
}

/// A mixer strip for track `i`, or the master.
fn strip(app: &mut App, ui: &mut Ui, i: Option<usize>, r: Rect) {
    let pal = app.palette;
    let selected = i.is_some() && app.selected_track == i;
    if selected {
        ui.painter().rect_filled(r, 0.0, pal.text.gamma_multiply(0.04));
    }
    let name = i.map_or("Master".to_owned(), |i| app.project.tracks[i].name.clone());
    let color = i.map_or(pal.accent, |i| pal.clip(i));
    ui.painter().rect_filled(Rect::from_min_size(r.min, vec2(r.width() - 1.0, 3.0)), 0.0, color);
    let p = ui.painter().with_clip_rect(r.shrink(2.0));
    p.text(pos2(r.center().x, r.top() + 14.0), Align2::CENTER_CENTER, &name, FontId::proportional(11.5), pal.text);
    if let Some(i) = i
        && ui.interact(Rect::from_min_size(r.min, vec2(r.width(), 26.0)), ui.scope_id().with(("strip name", i)), Sense::click()).clicked()
    {
        app.selected_track = Some(i);
    }

    // Pan, then mute/solo/arm, at the top; the fader fills the rest.
    let mut y = r.top() + 28.0;
    if let Some(i) = i {
        let mut pan = app.project.tracks[i].pan;
        let pr = Rect::from_min_size(pos2(r.left() + 8.0, y), vec2(r.width() - 16.0, 16.0));
        let resp = ui.interact(pr, ui.scope_id().with(("pan", i)), Sense::click_and_drag());
        ui.painter().rect_filled(pr.shrink2(vec2(0.0, 6.0)), 2.0, pal.well);
        let cx = pr.center().x + pan * pr.width() * 0.5;
        ui.painter().line_segment([pos2(pr.center().x, pr.center().y), pos2(cx, pr.center().y)], Stroke::new(3.0, pal.accent));
        ui.painter().circle_filled(pos2(cx, pr.center().y), 4.5, pal.text);
        if resp.double_clicked() {
            pan = 0.0;
        } else if resp.dragged() {
            pan = (pan + resp.drag_delta().x / (pr.width() * 0.5)).clamp(-1.0, 1.0);
        }
        let pan_text = if pan.abs() < 0.005 { "C".to_owned() } else if pan < 0.0 { format!("L{:.0}", -pan * 100.0) } else { format!("R{:.0}", pan * 100.0) };
        let resp = resp.on_hover_text(format!("Pan {pan_text} · double-click to center"));
        if pan != app.project.tracks[i].pan {
            app.project.tracks[i].pan = pan;
            app.mix_changed();
        }
        let _ = resp;
        y += 20.0;
        let mut bui = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(pos2(r.left() + 6.0, y), vec2(r.width() - 12.0, 20.0))));
        bui.spacing_mut().item_spacing.x = 3.0;
        let mut changed = false;
        let t = &mut app.project.tracks[i];
        bui.horizontal(|ui| {
            changed |= toggle(ui, &mut t.mute, "M", "Mute", pal.weak, &pal).clicked();
            changed |= toggle(ui, &mut t.solo, "S", "Solo", Color32::from_rgb(0xf2, 0xc1, 0x4e), &pal).clicked();
            let mut armed = t.armed;
            if toggle(ui, &mut armed, "R", "Record onto this track", REC, &pal).clicked() {
                app.queue.push("track arm".into());
            }
        });
        if changed {
            app.mix_changed();
        }
        if bui.min_rect().contains(ui.input(|i| i.pointer.interact_pos()).unwrap_or_default()) && ui.input(|i| i.pointer.any_click()) {
            app.selected_track = Some(i);
        }
        y += 24.0;
    } else {
        y += 44.0;
    }

    // The level readout under the fader, then the fader and meter.
    let gain_db = i.map_or(app.project.master_db, |i| app.project.tracks[i].gain_db);
    let bottom = r.bottom() - 22.0;
    p.text(pos2(r.center().x, bottom + 11.0), Align2::CENTER_CENTER, db_text(gain_db), FontId::monospace(10.5), pal.weak);
    if bottom - y < 30.0 {
        return;
    }
    let track = Rect::from_x_y_ranges(r.center().x - 14.0..=r.center().x + 2.0, y + 6.0..=bottom - 4.0);
    let meter = Rect::from_x_y_ranges(r.center().x + 8.0..=r.center().x + 14.0, track.y_range());
    ui.painter().rect_filled(track.shrink2(vec2(6.0, 0.0)), 2.0, pal.well);
    ui.painter().rect_filled(meter, 1.0, pal.well);
    // 0 dB, marked.
    let y0 = track.bottom() - fader_pos(0.0) * track.height();
    ui.painter().line_segment([pos2(track.left() - 2.0, y0), pos2(meter.right() + 2.0, y0)], Stroke::new(1.0, pal.line));
    let level = app.levels.get(i.unwrap_or(crate::engine::METERS)).copied().unwrap_or(0.0);
    if level > 0.0 {
        let h = fader_pos(db(level)) * meter.height();
        ui.painter().rect_filled(Rect::from_x_y_ranges(meter.x_range(), meter.bottom() - h..=meter.bottom()), 1.0, meter_color(level, &pal));
    }
    let id = ui.scope_id().with(("fader", i));
    let resp = ui.interact(track.expand2(vec2(8.0, 6.0)), id, Sense::click_and_drag());
    let mut pos = fader_pos(gain_db);
    if resp.double_clicked() {
        pos = fader_pos(0.0);
    } else if resp.dragged() {
        // Fine control with shift.
        let fine = if ui.input(|i| i.modifiers.shift) { 0.2 } else { 1.0 };
        pos = (pos - resp.drag_delta().y * fine / track.height()).clamp(0.0, 1.0);
    }
    let knob_y = track.bottom() - pos * track.height();
    let knob = Rect::from_center_size(pos2(track.center().x, knob_y), vec2(24.0, 10.0));
    ui.painter().rect_filled(knob, 2.0, if resp.dragged() || resp.hovered() { pal.text } else { pal.text.gamma_multiply(0.8) });
    ui.painter().line_segment([pos2(knob.left() + 3.0, knob_y), pos2(knob.right() - 3.0, knob_y)], Stroke::new(1.0, pal.panel));
    resp.on_hover_text("Drag to set the level (shift: finer) · double-click for 0 dB");
    let new_db = if (pos - fader_pos(gain_db)).abs() < 1e-6 { gain_db } else { (fader_db(pos) * 10.0).round() / 10.0 };
    if new_db != gain_db {
        match i {
            Some(i) => app.project.tracks[i].gain_db = new_db,
            None => app.project.master_db = new_db,
        }
        app.mix_changed();
    }
}

fn transport(app: &mut App, ui: &mut Ui) {
    let pal = app.palette;
    let playing = app.engine.playing();
    let recording = app.recording.is_some();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (icon, cmd, tip) in [
            (Icon::Start, "start", "Start"),
            (Icon::Back, "cut previous", "Previous clip edge"),
            (if playing && !recording { Icon::Pause } else { Icon::Play }, "play", "Play or pause (space)"),
            (Icon::Record, "record", "Record onto the armed track (r)"),
            (Icon::Forward, "cut next", "Next clip edge"),
            (Icon::End, "end", "End"),
        ] {
            let main = matches!(icon, Icon::Play | Icon::Pause);
            let lit = matches!(icon, Icon::Record) && recording;
            if icon_button(ui, icon, main, lit, &pal).on_hover_text(tip).clicked() {
                app.queue.push(cmd.into());
            }
        }
        ui.add_space(14.0);
        ui.label(RichText::new(timecode(app.playhead())).monospace().size(15.0).color(if recording { REC } else { pal.text }));
        ui.label(RichText::new(format!(" / {}", timecode(app.duration()))).monospace().size(12.0).color(pal.weak));
        if let Some(r) = &app.recording {
            ui.add_space(12.0);
            ui.label(RichText::new(format!("● REC {}", short_time(r.seconds()))).size(12.0).color(REC));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (label, cmd, tip) in [("Delete", "delete", "Remove the selected clip"), ("Split", "split", "Cut at the playhead (s)")] {
                if ui.button(label).on_hover_text(tip).clicked() {
                    app.queue.push(cmd.into());
                }
            }
        });
    });
}

fn media(app: &mut App, ui: &mut Ui) {
    if app.project.media.is_empty() {
        ui.add_space(8.0);
        ui.weak("Drop sound files here, or Import.");
        return;
    }
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for i in 0..app.project.media.len() {
            let m = &app.project.media[i];
            let state = if app.sounds.contains_key(&m.path) {
                String::new()
            } else if app.reading.contains(&m.path) {
                " · reading…".into()
            } else {
                " · missing".into()
            };
            let rate = if m.rate > 0 { format!(" · {} kHz", f64::from(m.rate) / 1000.0) } else { String::new() };
            let ch = match m.channels {
                0 => "",
                1 => " · mono",
                2 => " · stereo",
                _ => " · surround",
            };
            let details = format!("{}{rate}{ch}{state}", short_time(m.duration));
            let name = m.name();
            let selected = app.selected_media == Some(i);
            let r = ui.dnd_drag_source(Id::unique(("media", i)), MediaDrag(i), |ui| media_row(ui, &name, &details, selected)).inner;
            if r.clicked() {
                app.selected_media = Some(i);
            }
            let at = app.playhead();
            if r.double_clicked() {
                app.selected_media = Some(i);
                app.place(i, app.selected_track, at);
            }
            r.on_hover_text("Double-click to put it on the selected track at the playhead, or drag it onto a track").context_menu(|ui| {
                if ui.button("Put on a new track").clicked() {
                    app.place(i, None, at);
                    ui.close();
                }
            });
        }
    });
}

fn media_row(ui: &mut Ui, name: &str, details: &str, selected: bool) -> egui::Response {
    let (rect, r) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    let v = ui.visuals();
    if selected {
        ui.painter().rect_filled(rect, 4.0, v.selection.bg_fill);
        ui.painter().rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 8.0), vec2(2.0, rect.height() - 16.0)), 1.0, v.selection.stroke.color);
    } else if r.hovered() {
        ui.painter().rect_filled(rect, 4.0, v.widgets.hovered.weak_bg_fill);
    }
    let p = ui.painter().with_clip_rect(rect.shrink(2.0).intersect(ui.clip_rect()));
    p.text(rect.left_top() + vec2(10.0, 5.0), Align2::LEFT_TOP, name, FontId::proportional(13.0), v.strong_text_color());
    p.text(rect.left_top() + vec2(10.0, 22.0), Align2::LEFT_TOP, details, FontId::proportional(11.0), v.weak_text_color());
    r
}

#[derive(Clone, Copy)]
enum Icon {
    Start,
    Back,
    Play,
    Pause,
    Record,
    Forward,
    End,
}

/// A flat button with a drawn icon: no font needs to have the symbol. The
/// `main` one (play) is a solid circle in the signal color; `lit` shows
/// record while recording.
fn icon_button(ui: &mut Ui, icon: Icon, main: bool, lit: bool, pal: &Palette) -> egui::Response {
    let size = if main { vec2(34.0, 34.0) } else { vec2(28.0, 28.0) };
    let (rect, r) = ui.allocate_exact_size(size, Sense::click());
    let c = rect.center();
    let p = ui.painter();
    let color = if main {
        let fill = if r.hovered() { pal.accent.gamma_multiply(0.85) } else { pal.accent };
        p.circle_filled(c, 16.0, fill);
        pal.on_accent
    } else {
        if r.hovered() || lit {
            p.rect_filled(rect, 5.0, if lit { REC.gamma_multiply(0.25) } else { ui.visuals().widgets.hovered.weak_bg_fill });
        }
        if r.hovered() { pal.text } else { pal.weak }
    };
    let tri = |dir: f32, x: f32, w: f32, h: f32| {
        let pts = vec![pos2(x - dir * w / 2.0, c.y - h / 2.0), pos2(x + dir * w / 2.0, c.y), pos2(x - dir * w / 2.0, c.y + h / 2.0)];
        egui::Shape::convex_polygon(pts, color, Stroke::NONE)
    };
    let bar = |x: f32, h: f32| egui::Shape::rect_filled(Rect::from_center_size(pos2(x, c.y), vec2(2.0, h)), 0.0, color);
    match icon {
        Icon::Play => {
            p.add(tri(1.0, c.x + 1.5, 10.0, 12.0));
        }
        Icon::Pause => {
            p.add(egui::Shape::rect_filled(Rect::from_center_size(pos2(c.x - 2.5, c.y), vec2(3.0, 11.0)), 0.0, color));
            p.add(egui::Shape::rect_filled(Rect::from_center_size(pos2(c.x + 2.5, c.y), vec2(3.0, 11.0)), 0.0, color));
        }
        Icon::Record => {
            p.circle_filled(c, 5.5, if lit || r.hovered() { REC } else { REC.gamma_multiply(0.75) });
        }
        Icon::Back => {
            p.add(tri(-1.0, c.x + 1.0, 6.0, 9.0));
            p.add(bar(c.x - 3.5, 9.0));
        }
        Icon::Forward => {
            p.add(tri(1.0, c.x - 1.0, 6.0, 9.0));
            p.add(bar(c.x + 3.5, 9.0));
        }
        Icon::Start => {
            p.add(bar(c.x - 5.0, 10.0));
            p.add(tri(-1.0, c.x - 0.5, 6.0, 10.0));
            p.add(tri(-1.0, c.x + 4.5, 6.0, 10.0));
        }
        Icon::End => {
            p.add(tri(1.0, c.x - 4.5, 6.0, 10.0));
            p.add(tri(1.0, c.x + 0.5, 6.0, 10.0));
            p.add(bar(c.x + 5.0, 10.0));
        }
    }
    r
}

fn clip(app: &mut App, ui: &mut Ui) {
    let Some((ti, ci)) = app.selected_clip.filter(|&(t, c)| app.project.tracks.get(t).is_some_and(|t| c < t.clips.len())) else {
        ui.weak("Select a clip on a track.");
        return;
    };
    let c = app.project.tracks[ti].clips[ci].clone();
    let Some(m) = app.project.media.get(c.media).cloned() else { return };
    let step = 0.001;
    ui.label(RichText::new(m.name()).strong());
    ui.weak(m.path.parent().map(|p| p.display().to_string()).unwrap_or_default());
    ui.add_space(4.0);
    let fmt = |v: f64, _: std::ops::RangeInclusive<usize>| timecode(v);
    let mut sort = false;
    egui::Grid::new("clip grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label("Starts at");
        let mut v = c.at;
        let r = ui.add(egui::DragValue::new(&mut v).speed(0.01).range(0.0..=f64::MAX).custom_formatter(fmt));
        sort |= v != c.at;
        edit(app, &r, v != c.at, |a| a.project.tracks[ti].clips[ci].at = v);
        ui.end_row();
        ui.label("In");
        let mut v = c.src_in;
        let r = ui.add(egui::DragValue::new(&mut v).speed(step).range(0.0..=c.src_out - step).custom_formatter(fmt));
        edit(app, &r, v != c.src_in, |a| a.project.tracks[ti].clips[ci].src_in = v);
        ui.end_row();
        ui.label("Out");
        let mut v = c.src_out;
        let r = ui.add(egui::DragValue::new(&mut v).speed(step).range(c.src_in + step..=m.duration.max(c.src_in + step)).custom_formatter(fmt));
        edit(app, &r, v != c.src_out, |a| a.project.tracks[ti].clips[ci].src_out = v);
        ui.end_row();
        ui.label("Length");
        ui.monospace(timecode(c.len()));
        ui.end_row();
        ui.label("Gain");
        let mut v = c.gain_db;
        let r = ui.add(egui::DragValue::new(&mut v).speed(0.1).range(-60.0..=24.0).suffix(" dB"));
        edit(app, &r, v != c.gain_db, |a| a.project.tracks[ti].clips[ci].gain_db = v);
        ui.end_row();
    });
    if sort {
        let i = app.project.tracks[ti].sort(ci);
        app.selected_clip = Some((ti, i));
    }
}

/// Applies an edit from a field, with one undo step per drag or typed value.
fn edit(app: &mut App, r: &egui::Response, changed: bool, f: impl FnOnce(&mut App)) {
    if r.drag_started() || (changed && !r.dragged()) {
        app.snapshot();
    }
    if changed {
        f(app);
        app.edited();
    }
}

fn export(app: &mut App, ui: &mut Ui) {
    // The format: settings.toml's, or one an adapter adds.
    let presets: Vec<(String, String, String)> = app.adapters.exports().map(|(a, e)| (a.to_owned(), e.id.clone(), e.label.clone())).collect();
    let s = &app.settings.export;
    let own = format!("{}{}", s.format.to_uppercase(), if matches!(s.format.as_str(), "wav" | "flac") { format!(" {}-bit", s.bits) } else { String::new() });
    let chosen = app.export_choice.as_ref().and_then(|(a, id)| presets.iter().find(|p| &p.0 == a && &p.1 == id)).map(|p| p.2.clone());
    let mut choice = app.export_choice.clone();
    egui::ComboBox::from_id_salt("export format")
        .selected_text(chosen.unwrap_or_else(|| own.clone()))
        .width(ui.available_width().min(220.0))
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut choice, None, own.as_str()).on_hover_text("The format in settings.toml");
            for (a, id, label) in &presets {
                ui.selectable_value(&mut choice, Some((a.clone(), id.clone())), label).on_hover_text(format!("From {a}"));
            }
        });
    app.export_choice = choice;
    ui.label(format!("Stereo · {} kHz · {}", f64::from(app.engine.rate) / 1000.0, timecode(app.duration())));
    if app.export_choice.is_none() && ui.link("Change in settings.toml").clicked() {
        app.queue.push("config".into());
    }
    ui.add_space(6.0);
    if let Some(job) = &app.export {
        let done = job.progress.lock().unwrap_or_else(|e| e.into_inner()).done;
        let frac = (done / job.total.max(0.001)).clamp(0.0, 1.0) as f32;
        let secs = job.started.elapsed().as_secs_f64();
        let eta = if frac > 0.02 { format!(" · {} left", short_time(secs / frac as f64 - secs)) } else { String::new() };
        ui.add(egui::ProgressBar::new(frac).text(format!("{:.0}%{eta}", frac * 100.0)));
        if ui.button("Cancel").clicked() {
            app.cancel_export();
        }
    } else {
        let can = app.duration() > 0.0;
        let button = egui::Button::new(RichText::new("Export Mix…").color(app.palette.on_accent)).fill(app.palette.accent).min_size(vec2(140.0, 28.0));
        if ui.add_enabled(can, button).clicked() {
            app.queue.push("export".into());
        }
        match &app.last_export {
            Some(Ok(s)) => {
                ui.weak(format!("Done: {s}"));
            }
            Some(Err(e)) => {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            None => {}
        }
    }
}

fn log_panel(ui: &mut Ui) {
    egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(true).show(ui, |ui| {
        let err = ui.visuals().error_fg_color;
        log::each(|is_err, text| {
            if is_err {
                ui.colored_label(err, text);
            } else {
                ui.weak(text);
            }
        });
    });
}

fn help(app: &mut App, ui: &mut Ui) {
    ui.weak("Click to run. Keys are set in keys.toml.");
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        egui::Grid::new("help").num_columns(2).striped(true).show(ui, |ui| {
            for (name, _, what) in keys::COMMANDS {
                if ui.add(egui::Button::new(crate::menu::label(name)).frame(false)).on_hover_text(format!("{what}  ·  \"{name}\" in keys.toml")).clicked() {
                    app.queue.push((*name).into());
                }
                let key = app.keymap.key_for(name).map(|k| crate::menu::shortcut_text(&k)).unwrap_or_default();
                ui.label(RichText::new(key).monospace().color(app.palette.weak));
                ui.end_row();
            }
        });
    });
}

/// The wave9000 mark at `left_center`, `w` wide: five bars like a
/// waveform, the tallest in the signal color. splice9000's mark is two
/// strips; this one is its sound.
pub fn mark(p: &egui::Painter, left_center: Pos2, w: f32, pal: &Palette) {
    let light = if pal.dark { Color32::from_rgb(0xe8, 0xe9, 0xeb) } else { Color32::from_rgb(0x2a, 0x2c, 0x31) };
    let heights = [0.32, 0.62, 1.0, 0.7, 0.4];
    let bar = w / 5.0;
    for (k, h) in heights.iter().enumerate() {
        let x = left_center.x + k as f32 * bar;
        let r = Rect::from_center_size(pos2(x + bar * 0.5, left_center.y), vec2(bar * 0.62, w * 0.7 * h));
        p.rect_filled(r, bar * 0.31, if k == 2 { pal.accent } else { light });
    }
}

/// Every adapter: what it adds, what it may do, and Remove; then built-in
/// ones that were removed, with Add Back.
fn adapters_panel(app: &mut App, ui: &mut Ui) {
    let pal = app.palette;
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        ui.label(RichText::new("Drop an adapter's zip on this window to install it.").small().color(pal.weak));
        ui.add_space(8.0);
        if app.adapters.all.is_empty() {
            ui.weak("No adapters yet.");
        }
        let mut toggles = Vec::new();
        for (i, a) in app.adapters.all.iter().enumerate() {
            let m = &a.manifest;
            if i > 0 {
                ui.separator();
            }
            ui.horizontal(|ui| {
                ui.label(RichText::new(&m.name).strong().color(pal.text));
                ui.label(RichText::new(&m.version).small().color(pal.weak));
                if a.builtin {
                    ui.label(RichText::new("BUILT IN").size(9.5).color(pal.accent));
                }
                if a.is_running() {
                    ui.label(RichText::new("running").size(9.5).color(pal.weak));
                }
            });
            if !m.description.is_empty() {
                ui.label(RichText::new(&m.description).color(pal.weak));
            }
            let can = if m.run.is_empty() {
                "Presets only: nothing runs".to_owned()
            } else if m.permissions.is_empty() {
                "No permissions".to_owned()
            } else {
                format!("Can {}", m.permissions.iter().map(|p| crate::adapters::describe(p)).collect::<Vec<_>>().join("; "))
            };
            ui.label(RichText::new(can).small().color(pal.weak));
            if !m.extra.exports.is_empty() {
                let list: Vec<&str> = m.extra.exports.iter().map(|e| e.label.as_str()).collect();
                ui.label(RichText::new(format!("Export formats: {}", list.join(", "))).small().color(pal.weak));
            }
            ui.horizontal_wrapped(|ui| {
                for c in &m.commands {
                    if ui.small_button(&c.label).clicked() {
                        app.queue.push(crate::adapters::command_name("adapter", &m.name, &c.id));
                    }
                }
                for p in &m.panels {
                    let kind = format!("adapter:{}/{}", m.name, p.id);
                    if ui.add(egui::Button::selectable(app.layout.contains(&kind), RichText::new(format!("{} panel", p.title)).small())).clicked() {
                        toggles.push((m.name.clone(), p.id.clone()));
                    }
                }
                let tip = if a.builtin { "Take it out of wave9000; Add Back brings it again" } else { "Move it to the Trash" };
                if ui.small_button("Remove").on_hover_text(tip).clicked() {
                    app.queue.push(format!("adapter-remove \"{}\"", m.name));
                }
                if !m.homepage.is_empty() && ui.small_button("Homepage").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&m.homepage));
                }
            });
        }
        for (a, id) in toggles {
            app.toggle_adapter_panel(&a, &id);
        }
        let removed = app.adapters.removed_builtins();
        if !removed.is_empty() {
            ui.add_space(10.0);
            ui.label(RichText::new("REMOVED").size(10.0).color(pal.weak));
            for m in removed {
                ui.horizontal(|ui| {
                    ui.label(&m.name);
                    if ui.small_button("Add Back").clicked() {
                        app.queue.push(format!("adapter-add-back \"{}\"", m.name));
                    }
                });
            }
        }
        ui.add_space(10.0);
        if ui.link("Install from a folder instead…").clicked() {
            app.queue.push("install adapter folder".into());
        }
    });
}

/// A panel an adapter fills with `panel.set`: lines of text, or buttons
/// that run its commands.
fn adapter_panel(app: &mut App, kind: &str, ui: &mut Ui) {
    let Some((name, _)) = crate::layout::adapter_panel(kind) else { return };
    let Some(i) = app.adapters.index(name) else {
        ui.weak(format!("{name} isn't installed."));
        return;
    };
    if !app.adapters.all[i].is_running() {
        if app.adapters.approved(i) {
            // Started once when its panel shows; after a crash, by hand.
            let tried = ui.ctx().data_mut(|d| std::mem::replace(d.get_temp_mut_or_default::<bool>(Id::unique(("started", kind))), true));
            if !tried || ui.button(format!("Start {name}")).clicked() {
                app.queue.push(format!("adapter-start \"{name}\""));
            }
        } else if ui.button(format!("Allow {name}…")).clicked() {
            app.queue.push(format!("adapter-start \"{name}\""));
        }
    }
    let lines = app.adapter_panels.get(kind).cloned().unwrap_or_default();
    egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
        for line in &lines {
            match line {
                serde_json::Value::String(t) => {
                    ui.label(t);
                }
                serde_json::Value::Object(o) => {
                    let text = o.get("text").and_then(|t| t.as_str()).unwrap_or_default();
                    match o.get("command").and_then(|c| c.as_str()) {
                        Some(c) => {
                            if ui.button(text).clicked() {
                                app.queue.push(crate::adapters::command_name("adapter", name, c));
                            }
                        }
                        None => {
                            ui.label(text);
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

/// The short message adapters and installs leave, at the bottom.
pub fn note(ui: &Ui, rect: Rect, app: &mut App) {
    let now = ui.ctx().input(|i| i.time);
    let Some((text, until)) = &app.note else { return };
    if now > *until {
        app.note = None;
        return;
    }
    let pal = app.palette;
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("note")));
    let galley = painter.layout(text.clone(), FontId::proportional(12.0), pal.text, (rect.width() - 80.0).max(200.0));
    let r = Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 26.0), galley.size() + vec2(28.0, 14.0));
    painter.rect_filled(r, 6.0, pal.header);
    painter.rect_stroke(r, 6.0, Stroke::new(1.0, pal.line), StrokeKind::Inside);
    painter.galley(r.min + vec2(14.0, 7.0), galley, pal.text);
}

pub fn edit_banner(ui: &Ui, rect: Rect, pal: &Palette) {
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("layout banner")));
    let text = "EDIT LAYOUT  ·  drag panels onto each other  ·  right-click to add or remove  ·  Esc when done";
    let galley = painter.layout_no_wrap(text.into(), FontId::proportional(11.0), pal.on_accent);
    let r = Rect::from_center_size(pos2(rect.center().x, rect.bottom() - 22.0), galley.size() + vec2(24.0, 12.0));
    painter.rect_filled(r, 6.0, pal.accent);
    painter.galley(r.min + vec2(12.0, 6.0), galley, pal.on_accent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecodes() {
        assert_eq!(timecode(0.0), "0:00.000");
        assert_eq!(timecode(61.5), "1:01.500");
        assert_eq!(timecode(3600.0 + 2.25), "1:00:02.250");
    }

    #[test]
    fn faders_and_meters_share_a_scale() {
        assert_eq!(fader_pos(6.0), 1.0);
        assert!((fader_pos(0.0) - 0.708).abs() < 0.01);
        assert_eq!(fader_pos(-120.0), 0.0);
        for d in [-48.0, -12.0, -3.0, 0.0, 4.5] {
            assert!((fader_db(fader_pos(d)) - d).abs() < 1e-3, "{d}");
        }
        assert_eq!(fader_db(0.0), -90.0);
        assert_eq!(db_text(-90.0), "−∞");
        assert_eq!(db_text(-3.04), "−3.0");
    }
}
