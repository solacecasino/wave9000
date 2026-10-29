//! The window's layout: a tree of splitters with panels at the leaves,
//! like foobar2000's. Splitters can always be dragged; in layout edit mode
//! panels can be dragged onto another panel's edge (to sit beside it) or
//! its middle (to swap), and right-clicked to add, replace or remove.
//! The tree is saved to layout.toml, which can also be edited by hand.

use egui::{Align2, CursorIcon, FontId, Id, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, vec2};
use serde::{Deserialize, Serialize};

use crate::theme::Palette;

/// Every kind of panel, with its title.
pub const PANELS: &[(&str, &str)] = &[
    ("tracks", "Tracks"),
    ("mixer", "Mixer"),
    ("media", "Media"),
    ("transport", "Transport"),
    ("clip", "Clip"),
    ("export", "Export"),
    ("log", "Log"),
    ("help", "Help"),
    ("adapters", "Adapters"),
];

pub fn title(kind: &str) -> &str {
    PANELS.iter().find(|p| p.0 == kind).map_or(kind, |p| p.1)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// Children side by side.
    Row,
    /// Children stacked.
    Column,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split: Option<Dir>,
    /// Each child's share of the space.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sizes: Vec<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
    /// Swap places.
    Middle,
}

impl Node {
    pub fn panel(kind: &str) -> Self {
        Self { panel: Some(kind.to_owned()), split: None, sizes: vec![], children: vec![] }
    }

    pub fn split(dir: Dir, parts: Vec<(f32, Node)>) -> Self {
        let (sizes, children) = parts.into_iter().unzip();
        Self { panel: None, split: Some(dir), sizes, children }
    }

    pub fn is_panel(&self) -> bool {
        self.children.is_empty()
    }

    /// Makes a hand-edited tree sound: sizes that add up, no empty or
    /// one-child splits. Returns false when nothing is left.
    pub fn tidy(&mut self) -> bool {
        if self.children.is_empty() {
            if self.panel.is_none() {
                return false;
            }
            self.split = None;
            self.sizes.clear();
            return true;
        }
        self.panel = None;
        let mut sizes = std::mem::take(&mut self.sizes);
        sizes.resize(self.children.len(), 0.0);
        let mut kept = Vec::new();
        for (mut child, size) in std::mem::take(&mut self.children).into_iter().zip(sizes) {
            if child.tidy() {
                kept.push((if size.is_finite() && size > 0.0 { size } else { 1.0 }, child));
            }
        }
        match kept.len() {
            0 => return false,
            1 => *self = kept.pop().expect("one child").1,
            _ => {
                let total: f32 = kept.iter().map(|k| k.0).sum();
                self.split.get_or_insert(Dir::Row);
                (self.sizes, self.children) = kept.into_iter().map(|(s, c)| (s / total, c)).unzip();
            }
        }
        true
    }

    pub fn get(&self, path: &[usize]) -> Option<&Node> {
        match path.split_first() {
            None => Some(self),
            Some((i, rest)) => self.children.get(*i)?.get(rest),
        }
    }

    fn get_mut(&mut self, path: &[usize]) -> Option<&mut Node> {
        match path.split_first() {
            None => Some(self),
            Some((i, rest)) => self.children.get_mut(*i)?.get_mut(rest),
        }
    }

    /// A panel of this kind is showing.
    pub fn contains(&self, kind: &str) -> bool {
        self.find(kind, &mut Vec::new())
    }

    /// Where the first panel of this kind is, in `path`.
    pub fn find(&self, kind: &str, path: &mut Vec<usize>) -> bool {
        if self.panel.as_deref() == Some(kind) {
            return true;
        }
        for (i, c) in self.children.iter().enumerate() {
            path.push(i);
            if c.find(kind, path) {
                return true;
            }
            path.pop();
        }
        false
    }

    /// Puts a new `kind` panel beside the one at `path`.
    pub fn insert(&mut self, path: &[usize], kind: &str, side: Side) {
        let Some(target) = self.get_mut(path) else { return };
        if side == Side::Middle {
            target.panel = Some(kind.to_owned());
            return;
        }
        let dir = if matches!(side, Side::Left | Side::Right) { Dir::Row } else { Dir::Column };
        let before = matches!(side, Side::Left | Side::Top);
        // Beside a panel inside a split going the same way: join that split.
        if let Some((&last, parent_path)) = path.split_last()
            && let Some(parent) = self.get_mut(parent_path)
            && parent.split == Some(dir)
        {
            let half = parent.sizes[last] / 2.0;
            parent.sizes[last] = half;
            let at = if before { last } else { last + 1 };
            parent.sizes.insert(at, half);
            parent.children.insert(at, Node::panel(kind));
            return;
        }
        let Some(target) = self.get_mut(path) else { return };
        let old = std::mem::replace(target, Node::panel(""));
        let new = Node::panel(kind);
        *target = if before {
            Node::split(dir, vec![(0.5, new), (0.5, old)])
        } else {
            Node::split(dir, vec![(0.5, old), (0.5, new)])
        };
    }

    /// Removes the panel at `path`; its space goes to its neighbours. The
    /// last panel stays.
    pub fn remove(&mut self, path: &[usize]) {
        let Some((&last, parent_path)) = path.split_last() else { return };
        let Some(parent) = self.get_mut(parent_path) else { return };
        if last >= parent.children.len() {
            return;
        }
        parent.children.remove(last);
        parent.sizes.remove(last);
        self.tidy();
    }

    /// Moves the panel at `from` beside (or in place of) the one at `to`.
    pub fn move_panel(&mut self, from: &[usize], to: &[usize], side: Side) {
        if from == to {
            return;
        }
        let (Some(a), Some(b)) = (self.get(from).and_then(|n| n.panel.clone()), self.get(to).and_then(|n| n.panel.clone())) else {
            return;
        };
        if side == Side::Middle {
            self.get_mut(from).expect("checked").panel = Some(b);
            self.get_mut(to).expect("checked").panel = Some(a);
            return;
        }
        // Mark the old place, insert the new one (which may shift paths),
        // then remove the mark wherever it ended up.
        const MARK: &str = "\u{0}moving";
        self.get_mut(from).expect("checked").panel = Some(MARK.into());
        self.insert(to, &a, side);
        let mut at = Vec::new();
        if self.find(MARK, &mut at) {
            self.remove(&at);
        }
    }
}

pub fn default_layout() -> Node {
    Node::split(
        Dir::Column,
        vec![
            (0.09, Node::panel("transport")),
            (
                0.61,
                Node::split(
                    Dir::Row,
                    vec![
                        (0.18, Node::panel("media")),
                        (0.64, Node::panel("tracks")),
                        (0.18, Node::split(Dir::Column, vec![(0.55, Node::panel("clip")), (0.45, Node::panel("export"))])),
                    ],
                ),
            ),
            (0.30, Node::panel("mixer")),
        ],
    )
}

#[derive(Serialize, Deserialize)]
struct File {
    root: Node,
}

const FILE_HEADER: &str = "# wave9000 layout. Rearranged by dragging panel headers and saved here;\n\
# it can be edited by hand too. A node is either `panel = \"<kind>\"` or a\n\
# `split = \"row\"|\"column\"` with `sizes` and `[[...children]]`.\n\
# Panels: tracks, mixer, media, transport, clip, export, log, help,\n\
# adapters, and adapters' own as `adapter:<name>/<panel id>`.\n\n";

pub fn to_toml(root: &Node) -> String {
    let body = toml::to_string(&File { root: root.clone() }).unwrap_or_default();
    format!("{FILE_HEADER}{body}")
}

pub fn from_toml(text: &str) -> Result<Node, String> {
    let mut root = toml::from_str::<File>(text).map_err(|e| e.message().to_owned())?.root;
    if root.tidy() { Ok(root) } else { Err("the layout has no panels".into()) }
}

/// Which part of a panel is being drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    /// The right end of its header: a few small controls.
    Header,
    Body,
}

/// What the layout did this frame.
#[derive(Default)]
pub struct Outcome {
    /// The tree changed and should be saved.
    pub changed: bool,
}

/// Interface state that lasts between frames.
#[derive(Default)]
pub struct LayoutUi {
    /// Every panel shows as a block to drag, even without headers.
    pub editing: bool,
    /// The panel being moved, and its title.
    dragging: Option<(Vec<usize>, String)>,
}

/// Panels are divided by a hairline; the grab area around it is wider.
const LINE: f32 = 1.0;
const GRAB: f32 = 4.0;
/// The smallest a panel gets when a divider is dragged.
const MIN: f32 = 48.0;
pub const HEADER_HEIGHT: f32 = 24.0;

/// Draws `root` in `rect`, calling `panel` for each panel's header controls
/// and contents.
pub struct Frame<'a> {
    pub palette: &'a Palette,
    pub headers: bool,
    pub panel: &'a mut dyn FnMut(&str, Part, &mut Ui),
    /// Each panel's title; adapters name their own panels.
    pub title: &'a dyn Fn(&str) -> String,
}

/// A panel an adapter adds: `adapter:<adapter name>/<panel id>`.
pub fn adapter_panel(kind: &str) -> Option<(&str, &str)> {
    kind.strip_prefix("adapter:")?.rsplit_once('/')
}

impl LayoutUi {
    pub fn show(&mut self, ui: &mut Ui, root: &mut Node, rect: Rect, f: &mut Frame<'_>) -> Outcome {
        let mut out = Outcome::default();
        let mut action = None;
        // The hairlines are this color showing between panels.
        ui.painter().rect_filled(rect, 0.0, f.palette.line);
        self.node(ui, root, rect, &mut Vec::new(), f, &mut out, &mut action);
        if let Some((_, title)) = &self.dragging
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("panel ghost")));
            let galley = painter.layout_no_wrap(title.to_uppercase(), FontId::proportional(10.5), f.palette.on_accent);
            let r = Rect::from_min_size(p + vec2(14.0, 10.0), galley.size() + vec2(16.0, 8.0));
            painter.rect_filled(r, 4.0, f.palette.accent);
            painter.galley(r.min + vec2(8.0, 4.0), galley, f.palette.on_accent);
        }
        if let Some(a) = action {
            match a {
                Action::Move(from, to, side) => root.move_panel(&from, &to, side),
                Action::Insert(at, kind, side) => root.insert(&at, &kind, side),
                Action::Remove(at) => root.remove(&at),
            }
            out.changed = true;
        }
        if !ui.input(|i| i.pointer.any_down()) {
            self.dragging = None;
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn node(&mut self, ui: &mut Ui, node: &mut Node, rect: Rect, path: &mut Vec<usize>, f: &mut Frame<'_>, out: &mut Outcome, action: &mut Option<Action>) {
        if node.is_panel() {
            let kind = node.panel.clone().unwrap_or_default();
            self.leaf(ui, &kind, rect, path, f, action);
            return;
        }
        let row = node.split == Some(Dir::Row);
        let n = node.children.len();
        let space = ((if row { rect.width() } else { rect.height() }) - LINE * (n - 1) as f32).max(1.0);
        let mut at = if row { rect.left() } else { rect.top() };
        let mut rects = Vec::with_capacity(n);
        for &s in &node.sizes {
            // Whole points, so hairlines land on pixels.
            let end = (at + s * space).round();
            rects.push(if row { Rect::from_x_y_ranges(at..=end, rect.y_range()) } else { Rect::from_x_y_ranges(rect.x_range(), at..=end) });
            at = end + LINE;
        }
        if let Some(last) = rects.last_mut() {
            // Rounding leaves the last one to fill whatever is left.
            if row { last.max.x = rect.right() } else { last.max.y = rect.bottom() }
        }
        for (i, child) in node.children.iter_mut().enumerate() {
            path.push(i);
            self.node(ui, child, rects[i], path, f, out, action);
            path.pop();
        }
        // Dividers last, so their grab area wins over the panels' edges.
        for (i, a) in rects.iter().take(n - 1).copied().enumerate() {
            let line = if row {
                Rect::from_x_y_ranges(a.right()..=a.right() + LINE, rect.y_range())
            } else {
                Rect::from_x_y_ranges(rect.x_range(), a.bottom()..=a.bottom() + LINE)
            };
            let grab = if row { line.expand2(vec2(GRAB, 0.0)) } else { line.expand2(vec2(0.0, GRAB)) };
            path.push(i);
            let id = Id::unique(("divider", path.as_slice()));
            path.pop();
            let r = ui.interact(grab, id, Sense::drag());
            if r.hovered() || r.dragged() {
                ui.ctx().set_cursor_icon(if row { CursorIcon::ResizeColumn } else { CursorIcon::ResizeRow });
                let shown = if row { line.expand2(vec2(0.5, 0.0)) } else { line.expand2(vec2(0.0, 0.5)) };
                ui.painter().rect_filled(shown, 0.0, f.palette.accent);
            }
            if r.dragged() {
                let d = if row { r.drag_delta().x } else { r.drag_delta().y } / space;
                let pair = node.sizes[i] + node.sizes[i + 1];
                let min = (MIN / space).min(pair / 2.0);
                let new = (node.sizes[i] + d).clamp(min, pair - min);
                node.sizes[i + 1] = pair - new;
                node.sizes[i] = new;
            }
            if r.drag_stopped() {
                out.changed = true;
            }
        }
    }

    fn leaf(&mut self, ui: &mut Ui, kind: &str, rect: Rect, path: &[usize], f: &mut Frame<'_>, action: &mut Option<Action>) {
        let pal = *f.palette;
        ui.painter().rect_filled(rect, 0.0, pal.panel);
        let known = PANELS.iter().any(|p| p.0 == kind) || adapter_panel(kind).is_some();
        let name = (f.title)(kind);
        let mut body = rect;
        if f.headers {
            let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_HEIGHT.min(rect.height())));
            body.min.y = header.max.y;
            ui.painter().rect_filled(header, 0.0, pal.header);
            let r = ui.interact(header, Id::unique(("header", path)), Sense::click_and_drag());
            if r.hovered() && self.dragging.is_none() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
            if r.drag_started() {
                self.dragging = Some((path.to_vec(), name.clone()));
            }
            let text = name.to_uppercase();
            let p = ui.painter().with_clip_rect(header.intersect(ui.clip_rect()));
            let color = if r.hovered() || r.dragged() { pal.text } else { pal.weak };
            p.text(header.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, text, FontId::proportional(10.5), color);
            r.context_menu(|ui| panel_menu(ui, path, action));
            if known {
                let controls = header.shrink2(vec2(6.0, 2.0));
                let mut hui = ui.new_child(UiBuilder::new().max_rect(controls).layout(egui::Layout::right_to_left(egui::Align::Center)).id_salt(("header ui", path)));
                hui.set_clip_rect(controls.intersect(ui.clip_rect()));
                (f.panel)(kind, Part::Header, &mut hui);
            }
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(body).id_salt(("panel", path)));
        child.set_clip_rect(body.intersect(ui.clip_rect()));
        if known {
            (f.panel)(kind, Part::Body, &mut child);
        } else {
            child.add_space(8.0);
            child.horizontal(|ui| {
                ui.add_space(10.0);
                ui.weak(format!("No panel called \"{kind}\". Right-click its header to replace it."));
            });
        }

        let fg = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::unique("layout overlay")));
        if self.editing {
            // Edit mode: the whole panel is a handle to drag, with a menu.
            fg.rect_filled(rect, 0.0, pal.panel.gamma_multiply(0.9));
            fg.rect_stroke(rect, 0.0, Stroke::new(1.0, pal.accent.gamma_multiply(0.5)), StrokeKind::Inside);
            fg.text(rect.center(), Align2::CENTER_CENTER, name.to_uppercase(), FontId::proportional(13.0), pal.text);
            let r = egui::Area::new(Id::unique(("layout panel", path)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .show(ui.ctx(), |ui| ui.allocate_rect(rect, Sense::click_and_drag()))
                .inner;
            if r.drag_started() {
                self.dragging = Some((path.to_vec(), name.clone()));
            }
            r.context_menu(|ui| panel_menu(ui, path, action));
        }
        // Being dragged over: show where it would go.
        if let Some((from, _)) = self.dragging.clone()
            && from != path
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
            && rect.contains(p)
        {
            let side = side_at(rect, p);
            let zone = zone(rect, side);
            fg.rect_filled(zone, 0.0, pal.accent.gamma_multiply(0.22));
            fg.rect_stroke(zone, 0.0, Stroke::new(2.0, pal.accent), StrokeKind::Inside);
            if ui.input(|i| i.pointer.any_released()) {
                *action = Some(Action::Move(from, path.to_vec(), side));
                self.dragging = None;
            }
        }
    }
}

fn panel_menu(ui: &mut Ui, path: &[usize], action: &mut Option<Action>) {
    for (label, side) in [("Add Left", Side::Left), ("Add Right", Side::Right), ("Add Above", Side::Top), ("Add Below", Side::Bottom), ("Replace With", Side::Middle)] {
        ui.menu_button(label, |ui| {
            for (k, t) in PANELS {
                if ui.button(*t).clicked() {
                    *action = Some(Action::Insert(path.to_vec(), (*k).to_owned(), side));
                    ui.close();
                }
            }
        });
    }
    ui.separator();
    if ui.add_enabled(!path.is_empty(), egui::Button::new("Remove")).clicked() {
        *action = Some(Action::Remove(path.to_vec()));
        ui.close();
    }
}

enum Action {
    Move(Vec<usize>, Vec<usize>, Side),
    Insert(Vec<usize>, String, Side),
    Remove(Vec<usize>),
}

/// Which part of `rect` the pointer is over: the middle third swaps, the
/// rest is whichever edge is nearest.
fn side_at(rect: Rect, p: Pos2) -> Side {
    let x = (p.x - rect.left()) / rect.width();
    let y = (p.y - rect.top()) / rect.height();
    if (1.0 / 3.0..2.0 / 3.0).contains(&x) && (1.0 / 3.0..2.0 / 3.0).contains(&y) {
        return Side::Middle;
    }
    let edges = [(x, Side::Left), (1.0 - x, Side::Right), (y, Side::Top), (1.0 - y, Side::Bottom)];
    edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).expect("four edges").1
}

fn zone(r: Rect, side: Side) -> Rect {
    let (w, h) = (r.width() / 2.0, r.height() / 2.0);
    match side {
        Side::Left => Rect::from_min_size(r.min, vec2(w, r.height())),
        Side::Right => Rect::from_min_size(r.min + vec2(w, 0.0), vec2(w, r.height())),
        Side::Top => Rect::from_min_size(r.min, vec2(r.width(), h)),
        Side::Bottom => Rect::from_min_size(r.min + vec2(0.0, h), vec2(r.width(), h)),
        Side::Middle => r.shrink2(vec2(r.width() / 3.0, r.height() / 3.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn kinds(n: &Node) -> String {
        match &n.panel {
            Some(p) => p.clone(),
            None => {
                let dir = if n.split == Some(Dir::Row) { "row" } else { "col" };
                format!("{dir}({})", n.children.iter().map(kinds).collect::<Vec<_>>().join(" "))
            }
        }
    }

    /// The tree these tests move panels around in.
    fn sample() -> Node {
        Node::split(
            Dir::Column,
            vec![
                (
                    0.6,
                    Node::split(
                        Dir::Row,
                        vec![
                            (0.22, Node::panel("media")),
                            (0.56, Node::panel("tracks")),
                            (0.22, Node::split(Dir::Column, vec![(0.55, Node::panel("clip")), (0.45, Node::panel("export"))])),
                        ],
                    ),
                ),
                (0.09, Node::panel("transport")),
                (0.31, Node::panel("mixer")),
            ],
        )
    }

    #[test]
    fn round_trips_through_toml() {
        let root = default_layout();
        let text = to_toml(&root);
        assert_eq!(from_toml(&text).unwrap(), root);
        assert_eq!(kinds(&root), "col(transport row(media tracks col(clip export)) mixer)");
        assert_eq!(from_toml(&to_toml(&sample())).unwrap(), sample());
    }

    #[test]
    fn hand_edits_are_tidied() {
        let t = from_toml("[root]\nsplit = \"row\"\nsizes = [3]\n[[root.children]]\npanel = \"tracks\"\n[[root.children]]\npanel = \"mixer\"\n[[root.children]]\nsplit = \"column\"\n").unwrap();
        assert_eq!(kinds(&t), "row(tracks mixer)");
        assert!((t.sizes.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert!(from_toml("[root]\nsplit = \"row\"").is_err());
        assert!(from_toml("nonsense").is_err());
    }

    #[test]
    fn inserts_beside() {
        let mut t = sample();
        // Same direction as the parent: joins it.
        t.insert(&[0, 1], "log", Side::Right);
        assert_eq!(kinds(&t), "col(row(media tracks log col(clip export)) transport mixer)");
        // Across: the panel becomes a split.
        t.insert(&[2], "help", Side::Left);
        assert_eq!(kinds(&t), "col(row(media tracks log col(clip export)) transport row(help mixer))");
        let sum: f32 = t.children[0].sizes.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
    }

    #[test]
    fn moves_swaps_and_removes() {
        let mut t = sample();
        t.move_panel(&[0, 0], &[2], Side::Middle);
        assert_eq!(kinds(&t), "col(row(mixer tracks col(clip export)) transport media)");
        t.move_panel(&[0, 2, 0], &[0, 1], Side::Left);
        assert_eq!(kinds(&t), "col(row(mixer clip tracks export) transport media)");
        t.remove(&[0, 3]);
        t.remove(&[1]);
        assert_eq!(kinds(&t), "col(row(mixer clip tracks) media)");
        let sum: f32 = t.sizes.iter().sum();
        assert!((sum - 1.0).abs() < 1e-6);
        let mut one = Node::panel("tracks");
        one.remove(&[]);
        assert_eq!(kinds(&one), "tracks");
    }

    #[test]
    fn edge_zones() {
        let r = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 300.0));
        assert_eq!(side_at(r, Pos2::new(150.0, 150.0)), Side::Middle);
        assert_eq!(side_at(r, Pos2::new(10.0, 150.0)), Side::Left);
        assert_eq!(side_at(r, Pos2::new(290.0, 100.0)), Side::Right);
        assert_eq!(side_at(r, Pos2::new(150.0, 5.0)), Side::Top);
        assert_eq!(side_at(r, Pos2::new(150.0, 299.0)), Side::Bottom);
    }

    /// Drags a panel by its header with real pointer events through egui,
    /// the way a mouse would.
    #[test]
    fn dragging_a_panel_moves_it() {
        let ctx = egui::Context::default();
        let mut root = sample();
        let mut lui = LayoutUi::default();
        let screen = Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 800.0));
        let frame = |events: Vec<egui::Event>, root: &mut Node, lui: &mut LayoutUi| {
            let input = egui::RawInput { screen_rect: Some(screen), events, ..Default::default() };
            let mut changed = false;
            let palette = Palette::new(&crate::settings::Look::default());
            let mut out = ctx.run_ui(input, |ui| {
                let mut f = Frame { palette: &palette, headers: true, panel: &mut |_, _, _| {}, title: &|k| title(k).to_owned() };
                changed |= lui.show(ui, root, screen, &mut f).changed;
            });
            out.textures_delta.clear();
            changed
        };
        let button = |pos: Pos2, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        // Media's header is top left; the mixer is the bottom third.
        let (from, to) = (pos2(100.0, 10.0), pos2(990.0, 700.0));
        frame(vec![], &mut root, &mut lui);
        frame(vec![egui::Event::PointerMoved(from)], &mut root, &mut lui);
        frame(vec![button(from, true)], &mut root, &mut lui);
        for k in 1..=10 {
            let p = from + (to - from) * (k as f32 / 10.0);
            frame(vec![egui::Event::PointerMoved(p)], &mut root, &mut lui);
        }
        assert!(frame(vec![button(to, false)], &mut root, &mut lui));
        assert_eq!(kinds(&root), "col(row(tracks col(clip export)) transport row(mixer media))");
    }
}
