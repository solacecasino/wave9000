//! Draws egui's shapes with the CPU, straight into the window's pixels.
//!
//! Each frame, every shape is turned into triangles on its own and given a
//! fingerprint. Shapes whose fingerprint was also there last frame are left
//! alone; only the screen tiles under shapes that appeared, disappeared or
//! changed are drawn again. Typing a letter redraws a line, not the window.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use egui::epaint::{ClippedShape, Primitive, TessellationOptions, Tessellator, Vertex};
use egui::{Color32, TextureFilter, TextureId, TexturesDelta};

/// A rectangle of pixels; `x1` and `y1` are one past the last pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl IRect {
    pub fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    pub fn intersect(&self, o: &IRect) -> IRect {
        IRect::new(self.x0.max(o.x0), self.y0.max(o.y0), self.x1.min(o.x1), self.y1.min(o.y1))
    }

    fn area(&self) -> i64 {
        if self.is_empty() { 0 } else { (self.x1 - self.x0) as i64 * (self.y1 - self.y0) as i64 }
    }
}

/// The window's pixels: `0xAARRGGBB`, `stride` pixels from one row to the next.
pub struct Target<'a> {
    pub px: &'a mut [u32],
    pub stride: usize,
    pub width: usize,
    pub height: usize,
}

struct Texture {
    width: usize,
    height: usize,
    /// Shared with egui's delta rather than copied: the text atlas can be
    /// 16 MB.
    image: Arc<egui::ColorImage>,
    linear: bool,
    /// Bumped when the whole texture is replaced, so shapes that use it
    /// count as changed even when their triangles are the same.
    version: u64,
}

/// egui's textures (the text atlas, the icon), kept in memory once and
/// shared by every window.
#[derive(Default)]
pub struct Textures {
    map: HashMap<TextureId, Texture>,
    versions: u64,
}

impl Textures {
    /// Adds and updates textures. Call before painting.
    pub fn set(&mut self, delta: &TexturesDelta) {
        for (id, d) in delta.set.iter().flat_map(|(id, ds)| ds.iter().map(move |d| (id, d))) {
            let egui::ImageData::Color(image) = &d.image;
            let [w, h] = image.size;
            let linear = d.options.magnification == TextureFilter::Linear;
            match d.pos {
                None => {
                    self.versions += 1;
                    self.map.insert(
                        *id,
                        Texture { width: w, height: h, image: image.clone(), linear, version: self.versions },
                    );
                }
                Some([x, y]) => {
                    let Some(t) = self.map.get_mut(id) else { continue };
                    let px = &mut Arc::make_mut(&mut t.image).pixels;
                    for row in 0..h {
                        let ty = y + row;
                        if ty >= t.height {
                            break;
                        }
                        let n = w.min(t.width.saturating_sub(x));
                        px[ty * t.width + x..ty * t.width + x + n].copy_from_slice(&image.pixels[row * w..row * w + n]);
                    }
                }
            }
        }
    }

    /// Drops textures egui is done with, and marks the delta handled. Call
    /// after painting.
    pub fn free(&mut self, delta: &mut TexturesDelta) {
        for id in &delta.free {
            self.map.remove(id);
        }
        delta.clear();
    }

    fn version(&self, id: TextureId) -> u64 {
        self.map.get(&id).map_or(0, |t| t.version)
    }
}

/// One shape, as triangles in pixel coordinates.
struct Item {
    hash: u64,
    /// What the shape can touch: its triangles' bounds within its clip.
    bounds: IRect,
    clip: IRect,
    meshes: Vec<egui::epaint::Mesh>,
}

/// Screen tiles are this many pixels a side.
const TILE: i32 = 64;

/// What one window drew last, so the next frame only redraws what changed.
#[derive(Default)]
pub struct Canvas {
    /// Fingerprint and bounds of each shape drawn last frame.
    last: Vec<(u64, IRect)>,
    size: (usize, usize),
    /// Damage of recent frames, newest first, for buffers that are reused
    /// every other frame (Wayland) rather than every frame.
    history: VecDeque<Vec<IRect>>,
}

pub struct Frame {
    pub shapes: Vec<ClippedShape>,
    pub pixels_per_point: f32,
    pub options: TessellationOptions,
    pub atlas_size: [usize; 2],
    pub clear: Color32,
}

impl Canvas {
    /// Draws a frame. `age` is how many frames ago the target's pixels were
    /// drawn (0: unknown, draw everything). Returns the rectangles that
    /// changed, empty when nothing did.
    pub fn paint(&mut self, frame: Frame, textures: &Textures, target: &mut Target<'_>, age: u8) -> Vec<IRect> {
        let screen = IRect::new(0, 0, target.width as i32, target.height as i32);
        let clear = pack(frame.clear.to_opaque());
        let items = tessellate(frame, textures, screen);

        let full = age == 0
            || self.size != (target.width, target.height)
            || age as usize > self.history.len() + 1;
        let mut damage = if full { vec![screen] } else { self.changes(&items) };
        // A frame with no changes isn't shown, so it doesn't count as one
        // of the buffer's frames either.
        if !damage.is_empty() {
            self.history.push_front(damage.clone());
            self.history.truncate(3);
            if !full {
                for older in self.history.iter().skip(1).take(age as usize - 1) {
                    damage.extend_from_slice(older);
                }
            }
        }
        self.size = (target.width, target.height);
        self.last = items.iter().map(|i| (i.hash, i.bounds)).collect();

        let rects = tiles(&damage, screen);
        for r in &rects {
            for y in r.y0..r.y1 {
                let row = y as usize * target.stride;
                target.px[row + r.x0 as usize..row + r.x1 as usize].fill(clear);
            }
            for item in &items {
                let area = item.clip.intersect(r);
                if area.is_empty() || item.bounds.intersect(r).is_empty() {
                    continue;
                }
                for mesh in &item.meshes {
                    if let Some(texture) = textures.map.get(&mesh.texture_id) {
                        draw_mesh(mesh, texture, area, target);
                    }
                }
            }
        }
        rects
    }

    /// Bounds of shapes that weren't drawn last frame, and of last frame's
    /// shapes that aren't drawn any more.
    fn changes(&self, items: &[Item]) -> Vec<IRect> {
        let mut old: HashMap<u64, Vec<IRect>> = HashMap::with_capacity(self.last.len());
        for (hash, bounds) in &self.last {
            old.entry(*hash).or_default().push(*bounds);
        }
        let mut damage = Vec::new();
        for item in items {
            match old.get_mut(&item.hash).and_then(Vec::pop) {
                Some(_) => {}
                None => damage.push(item.bounds),
            }
        }
        damage.extend(old.into_values().flatten());
        damage
    }
}

fn tessellate(frame: Frame, textures: &Textures, screen: IRect) -> Vec<Item> {
    let ppp = frame.pixels_per_point;
    let mut tessellator = Tessellator::new(ppp, frame.options, frame.atlas_size, Vec::new());
    let mut items = Vec::with_capacity(frame.shapes.len());
    let mut primitives = Vec::new();
    for shape in frame.shapes {
        primitives.clear();
        tessellator.tessellate_clipped_shape(shape, &mut primitives);
        for p in primitives.drain(..) {
            let Primitive::Mesh(mut mesh) = p.primitive else { continue };
            if mesh.is_empty() {
                continue;
            }
            // The same rounding as egui's GPU painters use for the scissor rect.
            let c = p.clip_rect;
            let clip = IRect::new(
                (c.min.x * ppp).round() as i32,
                (c.min.y * ppp).round() as i32,
                (c.max.x * ppp).round() as i32,
                (c.max.y * ppp).round() as i32,
            )
            .intersect(&screen);
            if clip.is_empty() {
                continue;
            }
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for v in &mut mesh.vertices {
                v.pos.x *= ppp;
                v.pos.y *= ppp;
                x0 = x0.min(v.pos.x);
                y0 = y0.min(v.pos.y);
                x1 = x1.max(v.pos.x);
                y1 = y1.max(v.pos.y);
            }
            let bounds =
                IRect::new(x0.floor() as i32, y0.floor() as i32, x1.ceil() as i32 + 1, y1.ceil() as i32 + 1).intersect(&clip);
            if bounds.is_empty() {
                continue;
            }
            let mut h = Hash::default();
            h.add(clip.x0 as u32);
            h.add(clip.y0 as u32);
            h.add(clip.x1 as u32);
            h.add(clip.y1 as u32);
            h.add_u64(match mesh.texture_id {
                TextureId::Managed(n) => n,
                TextureId::User(n) => n ^ (1 << 63),
            });
            h.add_u64(textures.version(mesh.texture_id));
            h.add_words(vertex_words(&mesh.vertices));
            h.add_words(&mesh.indices);
            // Shapes split into several meshes become one item each.
            items.push(Item { hash: h.0, bounds, clip, meshes: vec![mesh] });
        }
    }
    items
}

fn vertex_words(v: &[Vertex]) -> &[u32] {
    // SAFETY: Vertex is #[repr(C)]: two Pos2 (two f32 each) and a Color32
    // ([u8; 4]), 20 bytes with no padding and 4-byte alignment.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u32>(), size_of_val(v) / 4) }
}

/// A quick fingerprint (not for security).
#[derive(Default)]
struct Hash(u64);

impl Hash {
    fn add_u64(&mut self, w: u64) {
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
    fn add(&mut self, w: u32) {
        self.add_u64(w as u64);
    }
    fn add_words(&mut self, words: &[u32]) {
        self.add(words.len() as u32);
        let (pairs, rest) = words.as_chunks::<2>();
        for p in pairs {
            self.add_u64(p[0] as u64 | (p[1] as u64) << 32);
        }
        for w in rest {
            self.add(*w);
        }
    }
}

/// Snaps damage to screen tiles and joins neighbours, so a burst of small
/// changes becomes a few rectangles.
fn tiles(damage: &[IRect], screen: IRect) -> Vec<IRect> {
    let cols = ((screen.x1 + TILE - 1) / TILE).max(0) as usize;
    let rows = ((screen.y1 + TILE - 1) / TILE).max(0) as usize;
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    let mut marked = vec![false; cols * rows];
    let mut any = false;
    for d in damage {
        let d = d.intersect(&screen);
        if d.is_empty() {
            continue;
        }
        any = true;
        for ty in (d.y0 / TILE) as usize..=((d.y1 - 1) / TILE) as usize {
            for tx in (d.x0 / TILE) as usize..=((d.x1 - 1) / TILE) as usize {
                marked[ty * cols + tx] = true;
            }
        }
    }
    if !any {
        return Vec::new();
    }
    // Runs of marked tiles in each row, then runs that line up in the next
    // row are merged downward.
    let mut out: Vec<IRect> = Vec::new();
    let mut open: Vec<usize> = Vec::new();
    for ty in 0..rows {
        let mut next_open = Vec::new();
        let mut tx = 0;
        while tx < cols {
            if !marked[ty * cols + tx] {
                tx += 1;
                continue;
            }
            let start = tx;
            while tx < cols && marked[ty * cols + tx] {
                tx += 1;
            }
            let (x0, x1) = (start as i32 * TILE, tx as i32 * TILE);
            let y0 = ty as i32 * TILE;
            match open.iter().find(|&&i| out[i].x0 == x0 && out[i].x1 == x1) {
                Some(&i) => {
                    out[i].y1 = y0 + TILE;
                    next_open.push(i);
                }
                None => {
                    out.push(IRect::new(x0, y0, x1, y0 + TILE));
                    next_open.push(out.len() - 1);
                }
            }
        }
        open = next_open;
    }
    for r in &mut out {
        *r = r.intersect(&screen);
    }
    // Many pieces covering most of the screen: one rectangle is cheaper.
    let covered: i64 = out.iter().map(IRect::area).sum();
    if out.len() > 24 && covered * 10 > screen.area() * 6 {
        let mut all = out[0];
        for r in &out[1..] {
            all = IRect::new(all.x0.min(r.x0), all.y0.min(r.y0), all.x1.max(r.x1), all.y1.max(r.y1));
        }
        return vec![all];
    }
    out
}

fn pack(c: Color32) -> u32 {
    0xff00_0000 | (c.r() as u32) << 16 | (c.g() as u32) << 8 | c.b() as u32
}

/// `x * a / 255`, rounded, for bytes.
#[inline(always)]
fn mul(x: u32, a: u32) -> u32 {
    let t = x * a + 128;
    (t + (t >> 8)) >> 8
}

/// Premultiplied `src` over the pixel `dst`.
#[inline(always)]
fn over(dst: &mut u32, src: [u32; 4]) {
    let a = src[3];
    if a == 0 && src[0] == 0 && src[1] == 0 && src[2] == 0 {
        return;
    }
    if a == 255 {
        *dst = 0xff00_0000 | src[0].min(255) << 16 | src[1].min(255) << 8 | src[2].min(255);
        return;
    }
    let inv = 255 - a;
    let d = *dst;
    let r = (src[0] + mul((d >> 16) & 0xff, inv)).min(255);
    let g = (src[1] + mul((d >> 8) & 0xff, inv)).min(255);
    let b = (src[2] + mul(d & 0xff, inv)).min(255);
    *dst = 0xff00_0000 | r << 16 | g << 8 | b;
}

#[inline(always)]
fn rgba(c: Color32) -> [u32; 4] {
    [c.r() as u32, c.g() as u32, c.b() as u32, c.a() as u32]
}

#[inline(always)]
fn tint(texel: Color32, color: [u32; 4]) -> [u32; 4] {
    [
        mul(texel.r() as u32, color[0]),
        mul(texel.g() as u32, color[1]),
        mul(texel.b() as u32, color[2]),
        mul(texel.a() as u32, color[3]),
    ]
}

impl Texture {
    #[inline(always)]
    fn texel(&self, x: i32, y: i32) -> Color32 {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.image.pixels[y * self.width + x]
    }

    /// The texture's color at normalized `(u, v)`, the way a GPU samples it.
    #[inline(always)]
    fn sample(&self, u: f32, v: f32) -> Color32 {
        let x = u * self.width as f32 - 0.5;
        let y = v * self.height as f32 - 0.5;
        if !self.linear {
            return self.texel((x + 0.5).floor() as i32, (y + 0.5).floor() as i32);
        }
        let (fx, fy) = (x.floor(), y.floor());
        let (tx, ty) = (x - fx, y - fy);
        let (ix, iy) = (fx as i32, fy as i32);
        if tx < 1.0 / 512.0 && ty < 1.0 / 512.0 {
            return self.texel(ix, iy);
        }
        let w = [(1.0 - tx) * (1.0 - ty), tx * (1.0 - ty), (1.0 - tx) * ty, tx * ty];
        let t = [self.texel(ix, iy), self.texel(ix + 1, iy), self.texel(ix, iy + 1), self.texel(ix + 1, iy + 1)];
        let ch = |c: fn(&Color32) -> u8| {
            (w[0] * c(&t[0]) as f32 + w[1] * c(&t[1]) as f32 + w[2] * c(&t[2]) as f32 + w[3] * c(&t[3]) as f32 + 0.5) as u8
        };
        Color32::from_rgba_premultiplied(ch(Color32::r), ch(Color32::g), ch(Color32::b), ch(Color32::a))
    }
}

/// The first and one-past-last pixel whose centers fall in `[lo, hi)`.
#[inline(always)]
fn span(lo: f32, hi: f32, min: i32, max: i32) -> (i32, i32) {
    let a = ((lo - 0.5).ceil() as i32).max(min);
    let b = ((hi - 0.5).ceil() as i32).min(max);
    (a, b)
}

fn draw_mesh(mesh: &egui::epaint::Mesh, texture: &Texture, area: IRect, target: &mut Target<'_>) {
    let v = &mesh.vertices;
    let idx = &mesh.indices;
    let mut i = 0;
    while i + 3 <= idx.len() {
        // egui draws rectangles (letters, flat fills) as two triangles,
        // corners [a b c, c b d]: those are filled a row at a time.
        if i + 6 <= idx.len()
            && idx[i + 3] == idx[i + 2]
            && idx[i + 4] == idx[i + 1]
            && draw_rect(
                [&v[idx[i] as usize], &v[idx[i + 1] as usize], &v[idx[i + 2] as usize], &v[idx[i + 5] as usize]],
                texture,
                area,
                target,
            )
        {
            i += 6;
            continue;
        }
        draw_triangle([&v[idx[i] as usize], &v[idx[i + 1] as usize], &v[idx[i + 2] as usize]], texture, area, target);
        i += 3;
    }
}

/// Fills an axis-aligned rectangle with one color, if `[a b c d]` is one
/// (corners top-left, top-right, bottom-left, bottom-right). Returns false
/// when it isn't, to draw it as triangles instead.
fn draw_rect(q: [&Vertex; 4], texture: &Texture, area: IRect, target: &mut Target<'_>) -> bool {
    let [a, b, c, d] = q;
    let straight = a.pos.y == b.pos.y
        && c.pos.y == d.pos.y
        && a.pos.x == c.pos.x
        && b.pos.x == d.pos.x
        && a.pos.x < b.pos.x
        && a.pos.y < c.pos.y
        && a.uv.y == b.uv.y
        && c.uv.y == d.uv.y
        && a.uv.x == c.uv.x
        && b.uv.x == d.uv.x
        && a.color == b.color
        && a.color == c.color
        && a.color == d.color;
    if !straight {
        return false;
    }
    let (x0, x1) = span(a.pos.x, b.pos.x, area.x0, area.x1);
    let (y0, y1) = span(a.pos.y, c.pos.y, area.y0, area.y1);
    if x0 >= x1 || y0 >= y1 {
        return true;
    }
    let color = rgba(a.color);
    let flat = a.uv == d.uv;
    let du = (b.uv.x - a.uv.x) / (b.pos.x - a.pos.x);
    let dv = (c.uv.y - a.uv.y) / (c.pos.y - a.pos.y);
    // A picture shown at its own size on whole pixels (the video preview):
    // copied texel for texel, with no filtering.
    let (tw, th) = (texture.width as f32, texture.height as f32);
    let whole = |x: f32| (x - x.round()).abs() < 0.01;
    if !flat
        && a.color == Color32::WHITE
        && (du * tw - 1.0).abs() < 1e-3
        && (dv * th - 1.0).abs() < 1e-3
        && whole(a.pos.x)
        && whole(a.pos.y)
        && whole(a.uv.x * tw)
        && whole(a.uv.y * th)
    {
        let ox = (a.uv.x * tw - a.pos.x).round() as i32;
        let oy = (a.uv.y * th - a.pos.y).round() as i32;
        for y in y0..y1 {
            let ty = (y + oy).clamp(0, texture.height as i32 - 1) as usize;
            let src = &texture.image.pixels[ty * texture.width..][..texture.width];
            let row = &mut target.px[y as usize * target.stride..][x0 as usize..x1 as usize];
            for (k, p) in row.iter_mut().enumerate() {
                let t = src[(x0 + k as i32 + ox).clamp(0, texture.width as i32 - 1) as usize];
                if t.a() == 255 {
                    *p = pack(t);
                } else {
                    over(p, rgba(t));
                }
            }
        }
        return true;
    }
    let solid = if flat { Some(tint(texture.sample(a.uv.x, a.uv.y), color)) } else { None };
    for y in y0..y1 {
        let row = &mut target.px[y as usize * target.stride..][x0 as usize..x1 as usize];
        match solid {
            Some(s) => {
                if s[3] == 255 {
                    row.fill(0xff00_0000 | s[0] << 16 | s[1] << 8 | s[2]);
                } else {
                    for p in row {
                        over(p, s);
                    }
                }
            }
            None => {
                let vv = a.uv.y + (y as f32 + 0.5 - a.pos.y) * dv;
                for (k, p) in row.iter_mut().enumerate() {
                    let u = a.uv.x + ((x0 + k as i32) as f32 + 0.5 - a.pos.x) * du;
                    over(p, tint(texture.sample(u, vv), color));
                }
            }
        }
    }
    true
}

fn draw_triangle(t: [&Vertex; 3], texture: &Texture, area: IRect, target: &mut Target<'_>) {
    let [p0, mut p1, mut p2] = t;
    let mut area2 = (p1.pos.x - p0.pos.x) * (p2.pos.y - p0.pos.y) - (p1.pos.y - p0.pos.y) * (p2.pos.x - p0.pos.x);
    if area2 == 0.0 || !area2.is_finite() {
        return;
    }
    if area2 < 0.0 {
        std::mem::swap(&mut p1, &mut p2);
        area2 = -area2;
    }
    let ymin = p0.pos.y.min(p1.pos.y).min(p2.pos.y);
    let ymax = p0.pos.y.max(p1.pos.y).max(p2.pos.y);
    let xmin = p0.pos.x.min(p1.pos.x).min(p2.pos.x);
    let xmax = p0.pos.x.max(p1.pos.x).max(p2.pos.x);
    let (y0, y1) = span(ymin, ymax + 1.0, area.y0, area.y1);
    let (bx0, bx1) = span(xmin, xmax + 1.0, area.x0, area.x1);
    if y0 >= y1 || bx0 >= bx1 {
        return;
    }

    // Edge k is opposite vertex k: e_k(x, y) = A x + B y + C, positive inside
    // and equal to the weight of vertex k times area2.
    let vs = [p0, p1, p2];
    let mut edges = [(0.0f32, 0.0f32, 0.0f32, false); 3];
    for k in 0..3 {
        let (s, e) = (vs[(k + 1) % 3].pos, vs[(k + 2) % 3].pos);
        let a = -(e.y - s.y);
        let b = e.x - s.x;
        let c = -(a * s.x + b * s.y);
        // Pixels exactly on an edge shared by two triangles go to one of them.
        let inclusive = a > 0.0 || (a == 0.0 && b < 0.0);
        edges[k] = (a, b, c, inclusive);
    }
    let same_color = p0.color == p1.color && p0.color == p2.color;
    let flat_uv = p0.uv == p1.uv && p0.uv == p2.uv;
    let solid = (same_color && flat_uv).then(|| tint(texture.sample(p0.uv.x, p0.uv.y), rgba(p0.color)));
    let inv = 1.0 / area2;

    for y in y0..y1 {
        let cy = y as f32 + 0.5;
        // The pixels of this row inside all three edges.
        let (mut lo, mut hi) = (bx0, bx1);
        for &(a, b, c, inclusive) in &edges {
            let k = b * cy + c;
            if a == 0.0 {
                if k < 0.0 || (k == 0.0 && !inclusive) {
                    lo = hi;
                }
                continue;
            }
            // a x + k >= 0 at pixel center x = px + 0.5.
            let bound = -k / a - 0.5;
            if a > 0.0 {
                let first = if inclusive { bound.ceil() } else { bound.floor() + 1.0 };
                lo = lo.max(first.max(i32::MIN as f32) as i32);
            } else {
                let last = if inclusive { bound.floor() } else { bound.ceil() - 1.0 };
                hi = hi.min((last + 1.0).min(i32::MAX as f32) as i32);
            }
        }
        if lo >= hi {
            continue;
        }
        let row = &mut target.px[y as usize * target.stride..][lo as usize..hi as usize];
        if let Some(s) = solid {
            if s[3] == 255 {
                row.fill(0xff00_0000 | s[0] << 16 | s[1] << 8 | s[2]);
            } else {
                for p in row {
                    over(p, s);
                }
            }
            continue;
        }
        let cx = lo as f32 + 0.5;
        let mut w = [0.0f32; 3];
        for k in 0..3 {
            let (a, b, c, _) = edges[k];
            w[k] = (a * cx + b * cy + c) * inv;
        }
        let dw = [edges[0].0 * inv, edges[1].0 * inv, edges[2].0 * inv];
        let cols = [rgba(p0.color), rgba(p1.color), rgba(p2.color)];
        for p in row {
            let (w0, w1, w2) = (w[0].max(0.0), w[1].max(0.0), w[2].max(0.0));
            let ch = |i: usize| (w0 * cols[0][i] as f32 + w1 * cols[1][i] as f32 + w2 * cols[2][i] as f32 + 0.5) as u32;
            let color = [ch(0), ch(1), ch(2), ch(3)];
            let src = if flat_uv {
                tint(texture.sample(p0.uv.x, p0.uv.y), color)
            } else {
                let u = w0 * p0.uv.x + w1 * p1.uv.x + w2 * p2.uv.x;
                let v = w0 * p0.uv.y + w1 * p1.uv.y + w2 * p2.uv.y;
                tint(texture.sample(u, v), color)
            };
            over(p, src);
            w[0] += dw[0];
            w[1] += dw[1];
            w[2] += dw[2];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::epaint::{Shape, StrokeKind};
    use egui::{Rect, Stroke, pos2};

    fn white_texture() -> Textures {
        let mut t = Textures::default();
        let image = egui::ColorImage::new([1, 1], vec![Color32::WHITE]);
        let mut delta = TexturesDelta::default();
        let full = egui::epaint::ImageDelta::full(image, egui::TextureOptions::LINEAR);
        delta.set.insert(TextureId::Managed(0), [full].into_iter().collect());
        t.set(&delta);
        t.free(&mut delta);
        t
    }

    fn frame(shapes: Vec<Shape>) -> Frame {
        Frame {
            shapes: shapes
                .into_iter()
                .map(|shape| ClippedShape { clip_rect: Rect::EVERYTHING, shape, transform_after_tessellation: Default::default() })
                .collect(),
            pixels_per_point: 1.0,
            options: TessellationOptions::default(),
            atlas_size: [1, 1],
            clear: Color32::WHITE,
        }
    }

    #[test]
    fn fills_rectangles_exactly() {
        let textures = white_texture();
        let mut px = vec![0u32; 100 * 100];
        let mut canvas = Canvas::default();
        let mut target = Target { px: &mut px, stride: 100, width: 100, height: 100 };
        let rect = Shape::rect_filled(Rect::from_min_max(pos2(10.0, 20.0), pos2(30.0, 40.0)), 0.0, Color32::RED);
        canvas.paint(frame(vec![rect]), &textures, &mut target, 0);
        assert_eq!(px[25 * 100 + 15], 0xffff0000);
        assert_eq!(px[25 * 100 + 5], 0xffffffff);
        assert_eq!(px[45 * 100 + 15], 0xffffffff);
    }

    #[test]
    fn redraws_only_what_changed() {
        let textures = white_texture();
        let mut px = vec![0u32; 512 * 512];
        let mut canvas = Canvas::default();
        let a = Shape::rect_filled(Rect::from_min_max(pos2(10.0, 10.0), pos2(20.0, 20.0)), 0.0, Color32::RED);
        let b = |x: f32| {
            Shape::rect_stroke(
                Rect::from_min_max(pos2(x, 300.0), pos2(x + 10.0, 310.0)),
                0.0,
                Stroke::new(1.0, Color32::BLUE),
                StrokeKind::Inside,
            )
        };
        let mut target = Target { px: &mut px, stride: 512, width: 512, height: 512 };
        let first = canvas.paint(frame(vec![a.clone(), b(300.0)]), &textures, &mut target, 0);
        assert_eq!(first, vec![IRect::new(0, 0, 512, 512)]);
        let same = canvas.paint(frame(vec![a.clone(), b(300.0)]), &textures, &mut target, 1);
        assert!(same.is_empty());
        let moved = canvas.paint(frame(vec![a, b(310.0)]), &textures, &mut target, 1);
        assert!(!moved.is_empty());
        assert!(moved.iter().all(|r| r.y0 >= 256 && r.x0 >= 256), "{moved:?}");
    }

    #[test]
    fn blends_premultiplied_colors() {
        let mut p = 0xff_ff_ff_ffu32;
        over(&mut p, [0, 0, 128, 128]);
        assert_eq!(p, 0xff_7f_7f_ff);
    }
}
