//! Window + event loop (winit). The OS main thread is the "browser main thread": it owns
//! wasm instance #0, services its mailbox, polls the controller and feeds KB_Input.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CursorGrabMode, Window, WindowId};

use crate::input::{self, Mapper, Pads};
use crate::{gl_ctx, log, rt};


pub fn quit(code: i32) -> ! {
    log::info(&format!("quitting ({code})"));
    log::flush();
    std::process::exit(code)
}

pub fn fatal(msg: &str) -> ! {
    log::error(msg);
    log::flush();
    std::process::exit(1)
}

pub struct App {
    pub egl_lib: PathBuf,
    boot: Option<rt::Boot>,
    window: Option<Window>,
    booted: bool,
    pads: Option<Pads>,
    mapper: Mapper,
    last_tick: Instant,
    grabbed: bool,
    started: Instant,
    autoplay: Option<f32>,
}

impl App {
    pub fn new(egl_lib: PathBuf, boot: rt::Boot) -> Self {
        Self {
            egl_lib,
            boot: Some(boot),
            window: None,
            booted: false,
            pads: None,
            mapper: Mapper::new(),
            last_tick: Instant::now(),
            grabbed: false,
            started: Instant::now(),
            // Debug: BO1Z_AUTOPLAY=<seconds> releases the intro gate without input.
            autoplay: std::env::var("BO1Z_AUTOPLAY").ok().and_then(|s| s.parse().ok()),
        }
    }

    fn start(&mut self, el: &ActiveEventLoop) -> anyhow::Result<()> {
        let mut attrs = Window::default_attributes().with_title("BO1Z");
        #[cfg(not(target_os = "ios"))]
        {
            attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(1600, 900));
        }
        #[cfg(target_os = "ios")]
        {
            attrs = attrs.with_fullscreen(Some(winit::window::Fullscreen::Borderless(None)));
        }
        let window = el.create_window(attrs)?;
        let native = native_window(&window)?;
        gl_ctx::create(&self.egl_lib, native)?;
        self.window = Some(window);
        self.pads = Some(Pads::new());
        log::info(&format!("window + EGL ready, surface {:?}", gl_ctx::surface_size()));
        if let Some(boot) = self.boot.take() {
            rt::start_browser_main(boot);
        }
        self.booted = true;
        Ok(())
    }

    fn send(&mut self, ev: (i32, i32, i32)) {
        if self.booted {
            rt::post_input(ev.0, ev.1, ev.2);
        }
    }

    fn set_grab(&mut self, on: bool) {
        let Some(w) = self.window.as_ref() else { return };
        if on {
            let ok = w.set_cursor_grab(CursorGrabMode::Locked).or_else(|_| w.set_cursor_grab(CursorGrabMode::Confined));
            if ok.is_ok() {
                w.set_cursor_visible(false);
                self.grabbed = true;
                self.send((input::LOCK, 1, 0));
            }
        } else {
            let _ = w.set_cursor_grab(CursorGrabMode::None);
            w.set_cursor_visible(true);
            self.grabbed = false;
            self.send((input::LOCK, 0, 0));
        }
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = (now - self.last_tick).as_secs_f32().min(0.1);
        self.last_tick = now;
        if !self.booted || !rt::ready() {
            return;
        }
        if let Some(secs) = self.autoplay {
            if self.started.elapsed().as_secs_f32() > secs {
                self.autoplay = None;
                log::info("autoplay: releasing intro");
                rt::post_input(input::INTRO, 1, 0);
                rt::post_input(input::LOCK, 1, 0);
            }
        }
        let Some(pads) = self.pads.as_mut() else { return };
        let pad = pads.poll();
        if !pad.connected {
            return;
        }
        let menu = rt::ui_state() & 1 != 0;
        let size = gl_ctx::surface_size();
        for ev in self.mapper.update(&pad, dt, menu, size) {
            rt::post_input(ev.0, ev.1, ev.2);
        }
    }
}

impl ApplicationHandler<()> for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.start(el) {
            fatal(&format!("startup failed: {e:?}"));
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _ev: ()) {}

    fn window_event(&mut self, _el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::CloseRequested => quit(0),
            WindowEvent::Focused(false) => {
                if self.grabbed {
                    self.set_grab(false);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if let PhysicalKey::Code(code) = event.physical_key {
                    if let Some(k) = input::winit_key(code) {
                        if !event.repeat {
                            self.send((input::KEY, k, (event.state == ElementState::Pressed) as i32));
                        }
                    }
                    if event.state == ElementState::Pressed && code == winit::keyboard::KeyCode::F11 {
                        self.set_grab(!self.grabbed);
                    }
                }
                if event.state == ElementState::Pressed {
                    if let Some(t) = event.text.as_ref() {
                        for c in t.chars().filter(|c| !c.is_control()) {
                            self.send((1, c as i32, 0));
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if !self.grabbed && state == ElementState::Pressed && button == MouseButton::Left {
                    self.send((input::INTRO, 1, 0));
                    self.set_grab(true);
                    return;
                }
                let k = match button {
                    MouseButton::Left => 200,
                    MouseButton::Right => 201,
                    MouseButton::Middle => 202,
                    MouseButton::Back => 203,
                    MouseButton::Forward => 204,
                    _ => return,
                };
                self.send((input::KEY, k, (state == ElementState::Pressed) as i32));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let y = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y,
                    winit::event::MouseScrollDelta::PixelDelta(p) => (p.y / 100.0) as f32,
                };
                if y != 0.0 {
                    let k = if y < 0.0 { 205 } else { 206 };
                    self.send((input::KEY, k, 1));
                    self.send((input::KEY, k, 0));
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if !self.grabbed {
                    self.send((input::CURSOR, position.x as i32, position.y as i32));
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, ev: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = ev {
            if self.grabbed {
                self.send((input::MOUSE, delta.0 as i32, delta.1 as i32));
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.tick();
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(8)));
    }
}

/// iOS: wasmtime JIT-compiles the game, which needs the process to be marked as debugged
/// (TrollStore "Open with JIT"). Log loudly when it is missing.
#[cfg(target_os = "ios")]
pub fn check_jit() {
    extern "C" {
        fn csops(pid: i32, ops: u32, useraddr: *mut u32, usersize: usize) -> i32;
        fn getpid() -> i32;
    }
    const CS_OPS_STATUS: u32 = 0;
    const CS_DEBUGGED: u32 = 0x1000_0000;
    let mut flags = 0u32;
    let rc = unsafe { csops(getpid(), CS_OPS_STATUS, &mut flags, 4) };
    let jit = rc == 0 && flags & CS_DEBUGGED != 0;
    log::info(&format!("code-signing flags {flags:#x} (rc {rc}); JIT {}", if jit { "AVAILABLE" } else { "NOT available" }));
    if !jit {
        log::error("JIT is not enabled. Long-press BO1Z in TrollStore and choose \"Open with JIT\".");
    }
}

#[cfg(not(target_os = "ios"))]
pub fn check_jit() {}

pub fn run(app: &mut App) -> anyhow::Result<()> {
    let el = EventLoop::<()>::with_user_event().build()?;
    el.run_app(app)?;
    Ok(())
}

#[cfg(not(target_os = "ios"))]
fn native_window(w: &Window) -> anyhow::Result<*mut std::ffi::c_void> {
    match w.window_handle()?.as_raw() {
        #[cfg(windows)]
        RawWindowHandle::Win32(h) => Ok(h.hwnd.get() as *mut _),
        other => anyhow::bail!("unsupported window handle {other:?}"),
    }
}

/// ANGLE's Metal backend accepts any CALayer: it adds a CAMetalLayer sublayer and keeps
/// it sized to the layer's bounds and scale.
#[cfg(target_os = "ios")]
fn native_window(w: &Window) -> anyhow::Result<*mut std::ffi::c_void> {
    use objc2::rc::Retained;
    use objc2_ui_kit::UIView;
    let RawWindowHandle::UiKit(h) = w.window_handle()?.as_raw() else {
        anyhow::bail!("expected a UIKit window handle");
    };
    unsafe {
        let view: &UIView = h.ui_view.cast::<UIView>().as_ref();
        let layer = view.layer();
        Ok(Retained::into_raw(layer) as *mut std::ffi::c_void)
    }
}
