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
    window::{Icon, Window, WindowId},
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
}

impl SoftwareApp {
    fn initialize(&mut self, event_loop: &ActiveEventLoop) -> Result<()> {
        let capture = self
            .launch
            .as_ref()
            .is_some_and(|launch| launch.capture_dir.is_some());
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Subscriptions Reminder")
                    .with_inner_size(LogicalSize::new(
                        1120.0,
                        if capture { 1000.0 } else { 900.0 },
                    ))
                    .with_min_inner_size(LogicalSize::new(860.0, 620.0))
                    .with_visible(!self.hidden)
                    .with_window_icon(Some(Icon::from_rgba(
                        crate::platform::icon_pixels(),
                        32,
                        32,
                    )?)),
            )?,
        );
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
        egui_winit::update_viewport_info(&mut state.info, &self.context, &state.window, false);
        let mut input = state.input.take_egui_input(&state.window);
        input
            .viewports
            .insert(egui::ViewportId::ROOT, state.info.clone());
        state.info.events.clear();
        let ui_started = Instant::now();
        let mut output = self.context.run(input, |ctx| state.native.draw(ctx));
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
        if matches!(
            event,
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. }
        ) {
            state.has_frame = false;
        }
        if matches!(event, WindowEvent::RedrawRequested) && state.has_frame {
            // An OS expose/paint event only needs the retained bitmap. Rebuilding
            // egui here would turn unrelated Windows paint traffic into UI frames.
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
        let pass = self.context.cumulative_pass_nr();
        if pass == repaint.pass || pass == repaint.pass + 1 {
            self.request_frame(repaint.when);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.platform.quit.load(Ordering::Acquire) {
            event_loop.exit();
            return;
        }
        if self.next_frame.is_some_and(|when| when <= Instant::now())
            && let Err(error) = self.draw(event_loop)
        {
            self.fail(event_loop, error);
        }
        event_loop.set_control_flow(
            self.next_frame
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
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error);
    }
    Ok(())
}
