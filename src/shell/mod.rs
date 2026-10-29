//! wave9000's window: winit for the window and input, egui for the interface,
//! and the CPU for drawing (see `raster`), in place of eframe and a GPU.
//!
//! A GPU path costs a graphics driver and two or three full-window buffers
//! on top of the app; drawing with the CPU into a single shared buffer, and
//! only where something changed, keeps wave9000's memory close to that of the
//! document itself.

mod present;
mod raster;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::{ViewportBuilder, ViewportCommand, ViewportId, ViewportIdPair, ViewportInfo, ViewportOutput};
use egui_winit::ActionRequested;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use present::Presenter;
use raster::{Canvas, Frame, Textures};

/// The largest texture egui may make. The text atlas grows to this width,
/// so it also caps how much memory the atlas can take.
const MAX_TEXTURE_SIDE: usize = 2048;

pub trait App {
    fn ui(&mut self, ui: &mut egui::Ui);

    /// Adjusts each frame's input before egui sees it.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, _raw_input: &mut egui::RawInput) {}
}

/// Opens the main window and runs until it closes.
pub fn run<A: App + 'static>(
    builder: ViewportBuilder,
    create: impl FnOnce(&egui::Context) -> A + 'static,
) -> Result<(), String> {
    let event_loop = EventLoop::<UserEvent>::with_user_event().build().map_err(|e| e.to_string())?;
    let mut runner = Runner {
        create: Some(Box::new(create)),
        builder,
        proxy: event_loop.create_proxy(),
        running: None,
        error: None,
    };
    event_loop.run_app(&mut runner).map_err(|e| e.to_string())?;
    runner.error.map_or(Ok(()), Err)
}

#[derive(Debug)]
enum UserEvent {
    Repaint { viewport: ViewportId, when: Instant, pass: u64 },
    AccessKit(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for UserEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::AccessKit(event)
    }
}

type Create<A> = Box<dyn FnOnce(&egui::Context) -> A>;

struct Runner<A> {
    create: Option<Create<A>>,
    builder: ViewportBuilder,
    proxy: EventLoopProxy<UserEvent>,
    running: Option<Running<A>>,
    error: Option<String>,
}

struct Running<A> {
    app: A,
    shared: Rc<RefCell<Shared>>,
}

struct Shared {
    ctx: egui::Context,
    start: Instant,
    proxy: EventLoopProxy<UserEvent>,
    viewports: HashMap<ViewportId, Viewport>,
    by_window: HashMap<WindowId, ViewportId>,
    textures: Textures,
    /// When each window next wants drawing.
    repaint_at: HashMap<ViewportId, Instant>,
    /// The main window asked to close and nothing canceled it.
    close: bool,
    /// When the main window last started a frame, and the screen's refresh
    /// interval.
    last_frame: Instant,
    frame_interval: Duration,
}

struct Viewport {
    ids: ViewportIdPair,
    builder: ViewportBuilder,
    info: ViewportInfo,
    window: Option<Arc<Window>>,
    input: Option<egui_winit::State>,
    presenter: Option<Presenter>,
    canvas: Canvas,
    commands: Vec<ViewportCommand>,
    actions: Vec<ActionRequested>,
    shown: bool,
}

thread_local! {
    /// The event loop, while one of its callbacks is running: immediate
    /// viewports (the Adapters window) open their window from inside egui.
    static EVENT_LOOP: Cell<*const ActiveEventLoop> = const { Cell::new(std::ptr::null()) };
}

fn with_event_loop<R>(event_loop: &ActiveEventLoop, f: impl FnOnce() -> R) -> R {
    let before = EVENT_LOOP.replace(event_loop);
    let result = f();
    EVENT_LOOP.set(before);
    result
}

fn current_event_loop<R>(f: impl FnOnce(&ActiveEventLoop) -> R) -> Option<R> {
    let ptr = EVENT_LOOP.get();
    // SAFETY: only set by with_event_loop, for as long as the reference lives.
    (!ptr.is_null()).then(|| f(unsafe { &*ptr }))
}

impl<A: App> ApplicationHandler<UserEvent> for Runner<A> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.running.is_some() {
            return;
        }
        with_event_loop(event_loop, || match self.start(event_loop) {
            // The window stays hidden until it has something to show, and
            // Windows never asks a hidden window to draw: draw the first
            // frame right away.
            Ok(()) => {
                if let Some(running) = &mut self.running {
                    running.frame(event_loop);
                }
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(running) = &mut self.running else { return };
        with_event_loop(event_loop, || running.window_event(event_loop, window_id, event));
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(running) = &mut self.running else { return };
        let mut shared = running.shared.borrow_mut();
        match event {
            UserEvent::Repaint { viewport, when, pass } => {
                // A request made before the latest frame was drawn is
                // already taken care of.
                let current = shared.ctx.cumulative_pass_nr_for(viewport);
                if current != pass && current != pass + 1 {
                    return;
                }
                // Animations ask for the next frame at once. A GPU waits for
                // the screen between frames; without one, they'd run as fast
                // as the processor allows, so they're paced to the screen.
                let when = when.max(shared.last_frame + shared.frame_interval);
                let at = shared.repaint_at.entry(viewport).or_insert(when);
                *at = (*at).min(when);
            }
            UserEvent::AccessKit(event) => {
                let Some(id) = shared.by_window.get(&event.window_id).copied() else { return };
                let Some(input) = shared.viewports.get_mut(&id).and_then(|v| v.input.as_mut()) else { return };
                match event.window_event {
                    accesskit_winit::WindowEvent::InitialTreeRequested => input.egui_ctx().enable_accesskit(),
                    accesskit_winit::WindowEvent::ActionRequested(request) => {
                        input.on_accesskit_action_request(request)
                    }
                    accesskit_winit::WindowEvent::AccessibilityDeactivated => {
                        input.egui_ctx().disable_accesskit();
                        return;
                    }
                }
                shared.repaint_at.insert(id, Instant::now());
            }
        }
        drop(shared);
        self.about_to_wait(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(running) = &self.running else { return };
        let mut shared = running.shared.borrow_mut();
        if shared.close {
            event_loop.exit();
            return;
        }
        let now = Instant::now();
        let mut next: Option<Instant> = None;
        let due = shared.repaint_at.values().any(|at| *at <= now);
        shared.repaint_at.retain(|_, at| *at > now);
        // Child windows are drawn during the main window's frame.
        if due && let Some(window) = shared.viewports.get(&ViewportId::ROOT).and_then(|v| v.window.as_ref()) {
            window.request_redraw();
        }
        for at in shared.repaint_at.values() {
            next = Some(next.map_or(*at, |n: Instant| n.min(*at)));
        }
        event_loop.set_control_flow(match next {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        });
    }
}

impl<A: App> Runner<A> {
    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let ctx = egui::Context::default();
        // Separate windows (Adapters) are real windows, and a frame may run
        // twice when wave9000 asks egui to redo it (after changing fonts).
        ctx.set_embed_viewports(false);
        ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::new(2).expect("2 is not zero"));
        let builder = self.builder.clone().with_visible(false);
        let window = Arc::new(egui_winit::create_window(&ctx, event_loop, &builder).map_err(|e| e.to_string())?);
        let mut info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut info, &ctx, &window, true);
        let ppp = egui_winit::pixels_per_point(&ctx, &window);
        ctx.input_mut(|i| {
            i.raw.viewports.insert(ViewportId::ROOT, info.clone());
            i.pixels_per_point = ppp;
        });

        let proxy = self.proxy.clone();
        ctx.set_request_repaint_callback(move |r| {
            let _ = proxy.send_event(UserEvent::Repaint {
                viewport: r.viewport_id,
                when: Instant::now() + r.delay,
                pass: r.current_cumulative_pass_nr,
            });
        });

        let mut input = new_input(&ctx, ViewportId::ROOT, event_loop, &window);
        input.init_accesskit(event_loop, &window, self.proxy.clone());

        let presenter = Presenter::new(window.clone())?;
        let create = self.create.take().ok_or("wave9000 was already started")?;
        let app = create(&ctx);

        let mut viewports = HashMap::new();
        viewports.insert(
            ViewportId::ROOT,
            Viewport {
                ids: ViewportIdPair::ROOT,
                builder,
                info,
                window: Some(window.clone()),
                input: Some(input),
                presenter: Some(presenter),
                canvas: Canvas::default(),
                commands: Vec::new(),
                actions: Vec::new(),
                shown: false,
            },
        );
        let shared = Rc::new(RefCell::new(Shared {
            ctx: ctx.clone(),
            start: Instant::now(),
            proxy: self.proxy.clone(),
            viewports,
            by_window: HashMap::from([(window.id(), ViewportId::ROOT)]),
            textures: Textures::default(),
            repaint_at: HashMap::from([(ViewportId::ROOT, Instant::now())]),
            close: false,
            last_frame: Instant::now(),
            frame_interval: frame_interval(&window),
        }));
        let weak: Weak<RefCell<Shared>> = Rc::downgrade(&shared);
        egui::Context::set_immediate_viewport_renderer(move |_ctx, viewport| {
            if let Some(shared) = weak.upgrade() {
                draw_immediate(&shared, viewport);
            }
        });
        self.running = Some(Running { app, shared });
        Ok(())
    }
}

/// How long the window's screen shows each frame (60 Hz if it won't say).
fn frame_interval(window: &Window) -> Duration {
    let millihertz = window.current_monitor().and_then(|m| m.refresh_rate_millihertz()).filter(|&mhz| mhz >= 10_000);
    Duration::from_secs_f64(1000.0 / millihertz.unwrap_or(60_000) as f64)
}

fn new_input(ctx: &egui::Context, id: ViewportId, event_loop: &ActiveEventLoop, window: &Window) -> egui_winit::State {
    egui_winit::State::new(
        ctx.clone(),
        id,
        event_loop,
        Some(window.scale_factor() as f32),
        event_loop.system_theme(),
        Some(MAX_TEXTURE_SIDE),
    )
}

impl<A: App> Running<A> {
    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let mut shared = self.shared.borrow_mut();
        let Some(id) = shared.by_window.get(&window_id).copied() else { return };
        let ctx = shared.ctx.clone();
        let Some(viewport) = shared.viewports.get_mut(&id) else { return };
        let parent = viewport.ids.parent;
        let mut shared_interval = None;

        match &event {
            WindowEvent::RedrawRequested => {
                drop(shared);
                if id == ViewportId::ROOT {
                    self.frame(event_loop);
                } else {
                    ctx.request_repaint_of(parent);
                }
                return;
            }
            WindowEvent::CloseRequested => {
                viewport.info.events.push(egui::ViewportEvent::Close);
                ctx.request_repaint_of(id);
                ctx.request_repaint_of(parent);
            }
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } | WindowEvent::Moved(_) => {
                if let Some(w) = &viewport.window {
                    if id == ViewportId::ROOT {
                        // It may be on another screen now.
                        let interval = frame_interval(w);
                        shared_interval = Some(interval);
                    }
                    if !matches!(event, WindowEvent::Moved(_)) {
                        w.request_redraw();
                    }
                }
            }
            _ => {}
        }
        if let Some(interval) = shared_interval.take() {
            shared.frame_interval = interval;
        }
        let Some(viewport) = shared.viewports.get_mut(&id) else { return };
        let (Some(window), Some(input)) = (&viewport.window, &mut viewport.input) else { return };
        let response = input.on_window_event(window, &event);
        if response.repaint {
            // Input for a child window is handled in its parent's frame.
            let target = if id == ViewportId::ROOT { id } else { parent };
            if let Some(w) = shared.viewports.get(&target).and_then(|v| v.window.as_ref()) {
                w.request_redraw();
            }
        }
    }

    /// Runs wave9000's interface for the main window and draws it.
    fn frame(&mut self, event_loop: &ActiveEventLoop) {
        let (ctx, raw_input) = {
            let mut shared = self.shared.borrow_mut();
            shared.last_frame = Instant::now();
            let Shared { ctx, viewports, start, .. } = &mut *shared;
            let ctx = ctx.clone();
            let infos: egui::ViewportIdMap<ViewportInfo> = {
                let root = viewports.get_mut(&ViewportId::ROOT).expect("the main window");
                if let Some(w) = &root.window {
                    egui_winit::update_viewport_info(&mut root.info, &ctx, w, false);
                }
                viewports.iter().map(|(id, v)| (*id, v.info.clone())).collect()
            };
            let root = viewports.get_mut(&ViewportId::ROOT).expect("the main window");
            let (Some(window), Some(input)) = (&root.window, &mut root.input) else { return };
            let mut raw = input.take_egui_input(window);
            raw.viewports = infos;
            raw.time = Some(start.elapsed().as_secs_f64());
            (ctx, raw)
        };
        let mut raw_input = raw_input;
        self.app.raw_input_hook(&ctx, &mut raw_input);
        let close_requested = raw_input.viewport().close_requested();

        // No borrow is held here: child windows draw during this call.
        let output = ctx.run_ui(raw_input, |ui| self.app.ui(ui));

        let mut shared = self.shared.borrow_mut();
        if close_requested
            && !output
                .viewport_output
                .get(&ViewportId::ROOT)
                .is_some_and(|o| o.commands.contains(&ViewportCommand::CancelClose))
        {
            shared.close = true;
            event_loop.exit();
        }
        if let Some(root) = shared.viewports.get_mut(&ViewportId::ROOT) {
            root.info.events.clear();
            if let (Some(window), Some(input)) = (&root.window, &mut root.input) {
                input.handle_platform_output_with_event_loop(window, event_loop, output.platform_output);
            }
        }
        shared.paint(ViewportId::ROOT, output.shapes, output.pixels_per_point, output.textures_delta);
        shared.apply_output(output.viewport_output, true);
    }
}

impl Shared {
    fn paint(
        &mut self,
        id: ViewportId,
        shapes: Vec<egui::epaint::ClippedShape>,
        pixels_per_point: f32,
        mut delta: egui::TexturesDelta,
    ) {
        self.textures.set(&delta);
        let options = self.ctx.tessellation_options(|o| *o);
        let atlas_size = self.ctx.fonts(|f| f.font_image_size());
        let clear = self.ctx.global_style().visuals.panel_fill;
        let Shared { viewports, textures, .. } = self;
        if let Some(v) = viewports.get_mut(&id)
            && let (Some(window), Some(presenter)) = (&v.window, &mut v.presenter)
        {
            let size = window.inner_size();
            let frame = Frame { shapes, pixels_per_point, options, atlas_size, clear };
            let canvas = &mut v.canvas;
            let started = Instant::now();
            let drawn = presenter.draw(window, size.width, size.height, |target, age| {
                let damage = canvas.paint(frame, textures, target, age);
                if timing() {
                    let pixels: i64 = damage.iter().map(|r| (r.x1 - r.x0) as i64 * (r.y1 - r.y0) as i64).sum();
                    eprintln!("frame: {} rects, {pixels} px, {:.2} ms", damage.len(), started.elapsed().as_secs_f64() * 1000.0);
                }
                damage
            });
            if let Err(e) = drawn {
                crate::log::error(format!("Couldn't draw the window: {e}"));
            }
            if !v.shown {
                v.shown = true;
                window.set_visible(true);
            }
            for action in v.actions.drain(..) {
                let Some(input) = &mut v.input else { continue };
                let event = match action {
                    ActionRequested::Cut => egui::Event::Cut,
                    ActionRequested::Copy => egui::Event::Copy,
                    ActionRequested::Paste => match input.clipboard_text() {
                        Some(text) if !text.is_empty() => egui::Event::Paste(text.replace("\r\n", "\n")),
                        _ => continue,
                    },
                    ActionRequested::Screenshot(_) | ActionRequested::PaintWhileHidden => continue,
                };
                input.egui_input_mut().events.push(event);
            }
        }
        self.textures.free(&mut delta);
    }

    /// Opens, updates and closes windows the way the frame asked. `all`:
    /// this was the main window's frame, which lists every window that's
    /// still wanted.
    fn apply_output(&mut self, outputs: egui::OrderedViewportIdMap<ViewportOutput>, all: bool) {
        let ctx = self.ctx.clone();
        for (id, out) in &outputs {
            let ids = ViewportIdPair::from_self_and_parent(*id, out.parent);
            let v = self.viewport(ids, out.builder.clone());
            v.commands.extend(out.commands.iter().cloned());
            if let Some(window) = &v.window {
                egui_winit::process_viewport_commands(
                    &ctx,
                    &mut v.info,
                    std::mem::take(&mut v.commands),
                    window,
                    &mut v.actions,
                );
            }
        }
        if all {
            self.viewports.retain(|id, _| outputs.contains_key(id));
            let alive = &self.viewports;
            self.by_window.retain(|_, id| alive.contains_key(id));
        }
    }

    /// The viewport for `ids`, made if it's new, with its builder updated.
    fn viewport(&mut self, ids: ViewportIdPair, builder: ViewportBuilder) -> &mut Viewport {
        let v = self.viewports.entry(ids.this).or_insert_with(|| Viewport {
            ids,
            builder: builder.clone(),
            info: ViewportInfo::default(),
            window: None,
            input: None,
            presenter: None,
            canvas: Canvas::default(),
            commands: Vec::new(),
            actions: Vec::new(),
            shown: false,
        });
        v.ids.parent = ids.parent;
        let (mut changes, recreate) = v.builder.patch(builder);
        if recreate {
            v.window = None;
            v.input = None;
            v.presenter = None;
            v.canvas = Canvas::default();
            v.shown = false;
        }
        v.commands.append(&mut changes);
        v
    }

    /// Gives a child viewport its window, if it has none yet.
    fn open(&mut self, id: ViewportId) {
        let ctx = self.ctx.clone();
        let proxy = self.proxy.clone();
        let Some(v) = self.viewports.get_mut(&id) else { return };
        if v.window.is_some() {
            return;
        }
        let made = current_event_loop(|event_loop| {
            let window = egui_winit::create_window(&ctx, event_loop, &v.builder.clone().with_visible(false)).ok()?;
            let window = Arc::new(window);
            let mut input = new_input(&ctx, id, event_loop, &window);
            input.init_accesskit(event_loop, &window, proxy);
            Some((window, input))
        })
        .flatten();
        let Some((window, input)) = made else { return };
        egui_winit::update_viewport_info(&mut v.info, &ctx, &window, true);
        match Presenter::new(window.clone()) {
            Ok(p) => v.presenter = Some(p),
            Err(e) => crate::log::error(format!("Couldn't open a window: {e}")),
        }
        self.by_window.insert(window.id(), id);
        v.window = Some(window);
        v.input = Some(input);
    }
}

/// Draws a child window (the Adapters window) during its parent's frame.
fn draw_immediate(shared: &RefCell<Shared>, viewport: egui::ImmediateViewport<'_>) {
    let egui::ImmediateViewport { ids, builder, mut viewport_ui_cb } = viewport;
    let (ctx, raw_input) = {
        let mut s = shared.borrow_mut();
        s.viewport(ids, builder);
        s.open(ids.this);
        let ctx = s.ctx.clone();
        let start = s.start;
        let infos: egui::ViewportIdMap<ViewportInfo> = s.viewports.iter().map(|(id, v)| (*id, v.info.clone())).collect();
        let Some(v) = s.viewports.get_mut(&ids.this) else { return };
        let (Some(window), Some(input)) = (&v.window, &mut v.input) else { return };
        egui_winit::update_viewport_info(&mut v.info, &ctx, window, false);
        let mut raw = input.take_egui_input(window);
        raw.viewports = infos;
        raw.time = Some(start.elapsed().as_secs_f64());
        (ctx, raw)
    };
    let output = ctx.run_ui(raw_input, |ui| viewport_ui_cb(ui));

    let mut s = shared.borrow_mut();
    if let Some(v) = s.viewports.get_mut(&ids.this) {
        v.info.events.clear();
        if let (Some(window), Some(input)) = (&v.window, &mut v.input) {
            input.handle_platform_output(window, output.platform_output);
        }
    }
    s.paint(ids.this, output.shapes, output.pixels_per_point, output.textures_delta);
    s.apply_output(output.viewport_output, false);
}

/// WAVE9000_FRAME_TIMES=1 prints how long each frame took to draw.
fn timing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("WAVE9000_FRAME_TIMES").is_some())
}
