//! CPU rendering with a GDI bitmap presentation on Windows. No graphics context.
use crate::{
    platform::Platform,
    storage::Store,
    ui::{App, Launch},
};
use anyhow::{Context, Result};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    fs::OpenOptions,
    io::Write,
    num::NonZeroU32,
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};
use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop, OwnedDisplayHandle},
    window::{Window, WindowId},
};

const FRAME_INTERVAL: Duration = Duration::from_micros(16_667);

struct Repaint {
    when: Instant,
    pass: u64,
}

struct SoftwareWindow {
    native: App,
    surface: softbuffer::Surface<OwnedDisplayHandle, Arc<Window>>,
    _context: softbuffer::Context<OwnedDisplayHandle>,
    window: Arc<Window>,
    input: egui_winit::State,
    info: egui::ViewportInfo,
    renderer: EguiSoftwareRender,
    theme: crate::theme::Preference,
    pending_textures: egui::TexturesDelta,
    pending_screenshots: Vec<egui::UserData>,
    scene: Vec<egui::epaint::ClippedShape>,
    pixels_per_point: f32,
    has_frame: bool,
}

struct SoftwareApp {
    store: Arc<Mutex<Store>>,
    platform: Arc<Platform>,
    launch: Option<Launch>,
    hidden: bool,
    context: egui::Context,
    window: Option<SoftwareWindow>,
    next_frame: Option<Instant>,
    last_frame: Option<Instant>,
    first_input: Option<Instant>,
    error: Option<anyhow::Error>,
    trace: Option<FrameTrace>,
    diagnostic: Option<Diagnostic>,
    counters: Counters,
}

impl SoftwareApp {
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        use winit::platform::windows::WindowAttributesExtWindows;
        let theme = self.store.lock().unwrap().settings.theme;
        let isolated_render = self.diagnostic.is_some()
            || self
                .launch
                .as_ref()
                .is_some_and(|launch| launch.capture_dir.is_some());
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title(if self.diagnostic.is_some() {
                        "Subscriptions Reminder — isolated diagnostic"
                    } else {
                        "Subscriptions Reminder"
                    })
                    .with_inner_size(LogicalSize::new(1000.0, 760.0))
                    .with_min_inner_size(LogicalSize::new(820.0, 580.0))
                    .with_active(!isolated_render)
                    .with_skip_taskbar(isolated_render)
                    .with_visible(!self.hidden),
            )?,
        );
        configure_window_icons(&window)?;
        if isolated_render {
            use winit::platform::windows::WindowExtWindows;
            window.set_enable(false);
        }
        // Keep winit subscribed to OS theme changes even after switching away
        // from a forced app theme. set_theme changes only the native appearance
        // in winit 0.30.13, not its initial preferred_theme subscription.
        if theme != crate::theme::Preference::System {
            window.set_theme(theme.window_theme());
        }
        let context = softbuffer::Context::new(event_loop.owned_display_handle())
            .map_err(|e| anyhow::anyhow!("Could not create the GDI drawing context: {e}"))?;
        let surface = softbuffer::Surface::new(&context, window.clone())
            .map_err(|e| anyhow::anyhow!("Could not create the GDI bitmap surface: {e}"))?;
        let input = egui_winit::State::new(
            self.context.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            window.theme(),
            None,
        );
        let hwnd = match window.window_handle()?.as_raw() {
            RawWindowHandle::Win32(handle) => handle.hwnd.get(),
            _ => anyhow::bail!("Only Windows is supported"),
        };
        let native = App::new(
            &self.context,
            self.store.clone(),
            self.platform.clone(),
            hwnd,
            self.launch
                .take()
                .context("The window is already initialized")?,
        );
        self.window = Some(SoftwareWindow {
            native,
            surface,
            _context: context,
            window,
            input,
            info: Default::default(),
            renderer: EguiSoftwareRender::new(ColorFieldOrder::Bgra),
            theme,
            pending_textures: Default::default(),
            pending_screenshots: Vec::new(),
            scene: Vec::new(),
            pixels_per_point: 0.0,
            has_frame: false,
        });
        self.request_frame(Instant::now());
        Ok(())
    }

    fn request_frame(&mut self, when: Instant) {
        let earliest = self
            .last_frame
            .map(|last| last + FRAME_INTERVAL)
            .unwrap_or(when);
        let when = when.max(earliest);
        self.next_frame = Some(self.next_frame.map_or(when, |pending| pending.min(when)));
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let started = Instant::now();
        let input_wait_ms = self
            .first_input
            .take()
            .map(|when| started.duration_since(when).as_secs_f64() * 1000.0);
        self.last_frame = Some(started);
        self.next_frame = None;
        let Some(state) = self.window.as_mut() else {
            return Ok(());
        };
        // Background results remain in their stores until the next visible frame.
        // Dispatch/quit stays live in the event loop without hidden egui layout.
        if !window_can_render(&state.window) {
            state.has_frame = false;
            return Ok(());
        }
        egui_winit::update_viewport_info(&mut state.info, &self.context, &state.window, false);
        let mut input = state.input.take_egui_input(&state.window);
        input
            .viewports
            .insert(egui::ViewportId::ROOT, state.info.clone());
        state.info.events.clear();
        self.counters.layouts += 1;
        if self.diagnostic.is_some() && self.counters.layouts > 40 {
            anyhow::bail!("Diagnostic exceeded its scene-construction bound.");
        }
        let ui_started = Instant::now();
        let mut output = self.context.run(input, |ctx| state.native.draw(ctx));
        let theme = state.native.theme_preference();
        if theme != state.theme {
            state.theme = theme;
            state.window.set_theme(theme.window_theme());
        }
        let ui_elapsed = ui_started.elapsed();
        // Keep font/image uploads from hidden frames until the next presentation.
        state.pending_textures.append(output.textures_delta);
        state
            .input
            .handle_platform_output(&state.window, output.platform_output);
        let mut close = false;
        let mut commands = Vec::new();
        let mut screenshots = Vec::new();
        if let Some(viewport) = output.viewport_output.get_mut(&egui::ViewportId::ROOT) {
            if viewport.repaint_delay != Duration::MAX {
                self.next_frame = started.checked_add(viewport.repaint_delay.max(FRAME_INTERVAL));
            }
            for command in viewport.commands.drain(..) {
                match command {
                    egui::ViewportCommand::Close => close = true,
                    egui::ViewportCommand::CancelClose => close = false,
                    egui::ViewportCommand::Screenshot(data) => screenshots.push(data),
                    command => commands.push(command),
                }
            }
        }
        let mut actions = Vec::new();
        egui_winit::process_viewport_commands(
            &self.context,
            &mut state.info,
            commands,
            &state.window,
            &mut actions,
        );
        state.pending_screenshots.extend(screenshots);
        if close || self.platform.quit.load(Ordering::Acquire) {
            self.platform.request_quit();
            event_loop.exit();
            return Ok(());
        }
        let visible =
            state.window.is_visible().unwrap_or(true) && state.window.is_minimized() != Some(true);
        let size = state.window.inner_size();
        if !visible || size.width == 0 || size.height == 0 {
            // Wakeups still process show/quit commands while hidden, without rasterizing.
            self.next_frame = None;
            state.has_frame = false;
            return Ok(());
        }
        if state.has_frame
            && state.pending_textures.is_empty()
            && state.pending_screenshots.is_empty()
            && state.pixels_per_point == output.pixels_per_point
            && state.scene == output.shapes
        {
            if let Some(trace) = &mut self.trace {
                trace.record(FrameSample {
                    painted: false,
                    total_ms: started.elapsed().as_secs_f64() * 1000.0,
                    ui_ms: ui_elapsed.as_secs_f64() * 1000.0,
                    raster_ms: 0.0,
                    present_ms: 0.0,
                    input_wait_ms,
                    at_ms: 0.0,
                });
            }
            return Ok(());
        }
        self.counters.rasters += 1;
        let raster_started = Instant::now();
        state
            .surface
            .resize(
                NonZeroU32::new(size.width).context("Missing bitmap width")?,
                NonZeroU32::new(size.height).context("Missing bitmap height")?,
            )
            .map_err(|e| anyhow::anyhow!("Could not resize the GDI bitmap: {e}"))?;
        state.scene = output.shapes;
        state.pixels_per_point = output.pixels_per_point;
        let primitives = self
            .context
            .tessellate(state.scene.clone(), output.pixels_per_point);
        let mut buffer = state
            .surface
            .buffer_mut()
            .map_err(|e| anyhow::anyhow!("Could not access the GDI bitmap: {e}"))?;
        buffer.fill(0);
        state.renderer.render(
            &mut BufferMutRef::new(
                bytemuck::cast_slice_mut(&mut buffer),
                size.width as usize,
                size.height as usize,
            ),
            &primitives,
            &state.pending_textures,
            output.pixels_per_point,
        );
        state.pending_textures = Default::default();
        if !state.pending_screenshots.is_empty() {
            let pixels = buffer
                .iter()
                .map(|pixel| {
                    egui::Color32::from_rgb((pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8)
                })
                .collect();
            let image = Arc::new(egui::ColorImage::new(
                [size.width as usize, size.height as usize],
                pixels,
            ));
            for data in state.pending_screenshots.drain(..) {
                state
                    .input
                    .egui_input_mut()
                    .events
                    .push(egui::Event::Screenshot {
                        viewport_id: egui::ViewportId::ROOT,
                        user_data: data,
                        image: image.clone(),
                    });
            }
            self.next_frame = Some(started + FRAME_INTERVAL);
        }
        let raster_elapsed = raster_started.elapsed();
        let present_started = Instant::now();
        buffer
            .present()
            .map_err(|e| anyhow::anyhow!("Could not display the GDI bitmap: {e}"))?;
        self.counters.presents += 1;
        state.has_frame = true;
        if let Some(trace) = &mut self.trace {
            trace.record(FrameSample {
                painted: true,
                total_ms: started.elapsed().as_secs_f64() * 1000.0,
                ui_ms: ui_elapsed.as_secs_f64() * 1000.0,
                raster_ms: raster_elapsed.as_secs_f64() * 1000.0,
                present_ms: present_started.elapsed().as_secs_f64() * 1000.0,
                input_wait_ms,
                at_ms: 0.0,
            });
        }
        Ok(())
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: anyhow::Error) {
        self.error = Some(error);
        event_loop.exit();
    }
}

impl ApplicationHandler<Repaint> for SoftwareApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none()
            && let Err(error) = self.initialize(event_loop)
        {
            self.fail(event_loop, error);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(state) = self.window.as_mut() else {
            return;
        };
        if state.window.id() != id {
            return;
        }
        if self.diagnostic.is_some()
            && matches!(
                event,
                WindowEvent::CursorEntered { .. }
                    | WindowEvent::CursorLeft { .. }
                    | WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::KeyboardInput { .. }
                    | WindowEvent::Ime(_)
                    | WindowEvent::Touch(_)
            )
        {
            return;
        }
        if matches!(event, WindowEvent::ThemeChanged(_))
            && state.theme != crate::theme::Preference::System
        {
            state.window.set_theme(state.theme.window_theme());
        }
        if matches!(
            event,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
        ) {
            state.has_frame = false;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            state.window.set_visible(false);
            state.has_frame = false;
            self.next_frame = None;
            return;
        }
        if !window_can_render(&state.window) {
            state.has_frame = false;
            self.next_frame = None;
            return;
        }
        if matches!(event, WindowEvent::ScaleFactorChanged { .. })
            && let Err(error) = configure_window_icons(&state.window)
        {
            self.fail(event_loop, error);
            return;
        }
        if matches!(event, WindowEvent::RedrawRequested) && state.has_frame {
            // An OS expose/paint event only needs the retained bitmap. Rebuilding
            // egui here would turn unrelated Windows paint traffic into UI frames.
            self.counters.cached_blits += 1;
            self.counters.presents += 1;
            let result = state
                .surface
                .buffer_mut()
                .and_then(|buffer| buffer.present());
            if let Err(error) = result {
                self.fail(
                    event_loop,
                    anyhow::anyhow!("Could not restore the GDI bitmap: {error}"),
                );
            }
            return;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            state.info.events.push(egui::ViewportEvent::Close);
        }
        let response = state.input.on_window_event(&state.window, &event);
        if response.repaint
            || matches!(
                event,
                WindowEvent::RedrawRequested | WindowEvent::CloseRequested
            )
        {
            self.first_input.get_or_insert_with(Instant::now);
            self.request_frame(Instant::now());
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, repaint: Repaint) {
        if self
            .window
            .as_ref()
            .is_none_or(|state| !window_can_render(&state.window))
        {
            self.counters.hidden_wakes += 1;
            self.next_frame = None;
            return;
        }
        let pass = self.context.cumulative_pass_nr();
        if pass == repaint.pass || pass == repaint.pass + 1 {
            self.request_frame(repaint.when);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.diagnostic_tick(event_loop);
        if self.platform.quit.load(Ordering::Acquire) || event_loop.exiting() {
            event_loop.exit();
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now()));
            return;
        }
        if self.next_frame.is_some_and(|when| when <= Instant::now())
            && let Err(error) = self.draw(event_loop)
        {
            self.fail(event_loop, error);
        }
        if event_loop.exiting() {
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now()));
            return;
        }
        event_loop.set_control_flow(
            [
                self.next_frame,
                self.diagnostic.as_ref().and_then(Diagnostic::deadline),
            ]
            .into_iter()
            .flatten()
            .min()
            .map_or(ControlFlow::Wait, ControlFlow::WaitUntil),
        );
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.context.set_request_repaint_callback(|_| {});
        if let Some(state) = &mut self.window {
            state.native.shutdown();
        }
    }
}

/// Opt-in performance-only batches are aggregated and written off the UI thread.
#[derive(Clone, Copy)]
struct FrameSample {
    painted: bool,
    total_ms: f64,
    ui_ms: f64,
    raster_ms: f64,
    present_ms: f64,
    input_wait_ms: Option<f64>,
    at_ms: f64,
}
struct FrameTrace {
    sender: Option<std::sync::mpsc::SyncSender<Vec<FrameSample>>>,
    worker: Option<std::thread::JoinHandle<()>>,
    started: Instant,
    epoch: Instant,
    frames: Vec<FrameSample>,
}
impl FrameTrace {
    fn new() -> Option<Self> {
        let path = std::env::var_os("SUBSCRIPTIONS_RENDER_TRACE")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        let (sender, receiver) = std::sync::mpsc::sync_channel::<Vec<FrameSample>>(4);
        let worker = std::thread::spawn(move || {
            let _ = writeln!(
                file,
                "{}",
                serde_json::json!({"renderer":"CPU / Windows GDI"})
            );
            for batch in receiver {
                if batch.is_empty() {
                    continue;
                }
                let mut total: Vec<_> = batch.iter().map(|frame| frame.total_ms).collect();
                total.sort_unstable_by(f64::total_cmp);
                let mean = |get: fn(&FrameSample) -> f64| {
                    batch.iter().map(get).sum::<f64>() / batch.len() as f64
                };
                let input: Vec<_> = batch
                    .iter()
                    .filter_map(|frame| frame.input_wait_ms)
                    .collect();
                let _ = writeln!(
                    file,
                    "{}",
                    serde_json::json!({
                        "ui_passes":batch.len(),
                        "painted_frames":batch.iter().filter(|frame| frame.painted).count(),
                        "unchanged_frames_skipped":batch.iter().filter(|frame| !frame.painted).count(),
                        "first_pass_at_ms":batch[0].at_ms,
                        "last_pass_at_ms":batch[batch.len()-1].at_ms,
                        "pass_times_ms":batch.iter().map(|frame| frame.at_ms).collect::<Vec<_>>(),
                        "frame_median_ms":total[total.len()/2],
                        "frame_p95_ms":total[(total.len()-1)*95/100], "frame_max_ms":total[total.len()-1],
                        "ui_mean_ms":mean(|frame| frame.ui_ms), "raster_mean_ms":mean(|frame| frame.raster_ms),
                        "present_mean_ms":mean(|frame| frame.present_ms),
                        "input_samples":input.len(), "input_wait_max_ms":input.into_iter().reduce(f64::max),
                    })
                );
            }
        });
        Some(Self {
            sender: Some(sender),
            worker: Some(worker),
            started: Instant::now(),
            epoch: Instant::now(),
            frames: Vec::with_capacity(64),
        })
    }
    fn record(&mut self, mut sample: FrameSample) {
        sample.at_ms = self.epoch.elapsed().as_secs_f64() * 1000.0;
        self.frames.push(sample);
        if self.started.elapsed() >= Duration::from_secs(1) || self.frames.len() >= 120 {
            self.publish();
        }
    }
    fn publish(&mut self) {
        if !self.frames.is_empty()
            && let Some(sender) = &self.sender
        {
            let _ = sender.try_send(std::mem::take(&mut self.frames));
        }
        self.started = Instant::now();
    }
}
impl Drop for FrameTrace {
    fn drop(&mut self) {
        self.publish();
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn run(
    store: Arc<Mutex<Store>>,
    platform: Arc<Platform>,
    launch: Launch,
    hidden: bool,
) -> std::result::Result<(), String> {
    run_inner(store, platform, launch, hidden)
        .map_err(|error| format!("Cannot run the application: {error:#}"))
}

fn run_inner(
    store: Arc<Mutex<Store>>,
    platform: Arc<Platform>,
    launch: Launch,
    hidden: bool,
) -> Result<()> {
    let event_loop = EventLoop::<Repaint>::with_user_event().build()?;
    let context = egui::Context::default();
    let proxy = event_loop.create_proxy();
    context.set_request_repaint_callback(move |info| {
        if let Some(when) = Instant::now().checked_add(info.delay) {
            let _ = proxy.send_event(Repaint {
                when,
                pass: info.current_cumulative_pass_nr,
            });
        }
    });
    let diagnostic = launch.diagnostic.clone().map(Diagnostic::new);
    let mut app = SoftwareApp {
        store,
        platform,
        launch: Some(launch),
        hidden,
        context,
        window: None,
        next_frame: None,
        last_frame: None,
        first_input: None,
        error: None,
        trace: FrameTrace::new(),
        diagnostic,
        counters: Counters::default(),
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error);
    }
    Ok(())
}

fn window_can_render(window: &Window) -> bool {
    let size = window.inner_size();
    window.is_visible().unwrap_or(true)
        && window.is_minimized() != Some(true)
        && size.width > 0
        && size.height > 0
}

fn configure_window_icons(window: &Window) -> Result<()> {
    use windows_sys::Win32::UI::{
        HiDpi::GetSystemMetricsForDpi,
        WindowsAndMessaging::{SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON},
    };
    use winit::{
        dpi::PhysicalSize,
        platform::windows::{IconExtWindows, WindowExtWindows},
        window::Icon,
    };
    let dpi = (window.scale_factor() * 96.0).round() as u32;
    let load = |x, y| {
        // SAFETY: Reads metric dimensions for this window DPI without pointers.
        let size = unsafe {
            PhysicalSize::new(
                GetSystemMetricsForDpi(x, dpi).max(1) as u32,
                GetSystemMetricsForDpi(y, dpi).max(1) as u32,
            )
        };
        Icon::from_resource(1, Some(size))
            .map_err(|error| anyhow::anyhow!("Cannot load application icon: {error}"))
    };
    window.set_window_icon(Some(load(SM_CXSMICON, SM_CYSMICON)?));
    window.set_taskbar_icon(Some(load(SM_CXICON, SM_CYICON)?));
    Ok(())
}

#[derive(Clone, Default, serde::Serialize)]
struct Counters {
    layouts: u64,
    rasters: u64,
    presents: u64,
    cached_blits: u64,
    hidden_wakes: u64,
}
struct Diagnostic {
    path: std::path::PathBuf,
    start: Instant,
    phase: usize,
    samples: Vec<serde_json::Value>,
}
const PHASES: &[(u64, &str)] = &[
    (2500, "settled_start"),
    (7500, "visible_idle_end"),
    (9500, "minimized_end"),
    (11500, "restored_settled"),
    (14500, "restored_idle_end"),
    (16500, "hidden_end"),
];
impl Diagnostic {
    fn new(path: std::path::PathBuf) -> Self {
        Self {
            path,
            start: Instant::now(),
            phase: 0,
            samples: vec![],
        }
    }
    fn deadline(&self) -> Option<Instant> {
        PHASES
            .get(self.phase)
            .map(|(ms, _)| self.start + Duration::from_millis(*ms))
    }
}
impl SoftwareApp {
    fn diagnostic_tick(&mut self, event_loop: &ActiveEventLoop) {
        let Some(diag) = &mut self.diagnostic else {
            return;
        };
        let Some((ms, state)) = PHASES.get(diag.phase) else {
            return;
        };
        if diag.start.elapsed() < Duration::from_millis(*ms) {
            return;
        }
        let Some(view) = &mut self.window else {
            return;
        };
        diag.samples.push(serde_json::json!({ "state": state,
            "elapsed_seconds":diag.start.elapsed().as_secs_f64(), "cpu_seconds":cpu_seconds(),
            "counters":self.counters, "window_visible":view.window.is_visible(),
            "window_minimized":view.window.is_minimized() }));
        let result = serde_json::json!({"backend":"egui CPU software rasterizer -> softbuffer Win32 GDI",
            "executable":std::env::current_exe().ok(), "pid":std::process::id(),
            "logical_cores":std::thread::available_parallelism().map_or(1, |n| n.get()),
            "loaded_graphics_modules":graphics_modules(), "samples":diag.samples});
        if let Some(parent) = diag.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(error) = std::fs::write(&diag.path, serde_json::to_vec_pretty(&result).unwrap())
        {
            self.fail(event_loop, error.into());
            return;
        }
        match diag.phase {
            1 => {
                view.window.set_minimized(true);
                view.has_frame = false;
                self.next_frame = None;
                self.context.request_repaint();
            }
            2 => {
                view.window.set_minimized(false);
                view.has_frame = false;
                self.next_frame = Some(Instant::now());
            }
            4 => {
                view.window.set_visible(false);
                view.has_frame = false;
                self.next_frame = None;
                self.context.request_repaint();
            }
            5 => {
                self.platform.quit.store(true, Ordering::Release);
                event_loop.exit();
            }
            _ => (),
        }
        diag.phase += 1;
    }
}
fn cpu_seconds() -> f64 {
    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    let mut times = [FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    }; 4];
    // SAFETY: Output pointers are valid and the process handle is this process.
    if unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut times[0],
            &mut times[1],
            &mut times[2],
            &mut times[3],
        )
    } == 0
    {
        return -1.0;
    }
    times[2..]
        .iter()
        .map(|t| {
            ((u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)) as f64 / 10_000_000.0
        })
        .sum()
}
fn graphics_modules() -> Vec<String> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    [
        "gdi32.dll",
        "opengl32.dll",
        "d3d11.dll",
        "d3d12.dll",
        "dxgi.dll",
        "vulkan-1.dll",
    ]
    .into_iter()
    .filter_map(|name| {
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: Reads module presence without loading any library.
        (!unsafe { GetModuleHandleW(wide.as_ptr()) }.is_null()).then(|| name.to_owned())
    })
    .collect()
}
