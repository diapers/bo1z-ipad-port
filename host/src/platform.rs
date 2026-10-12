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

use crate::input::{self, FakePad, Mapper, Pads};
use crate::maps::MapInfo;
use crate::menu::Menu;
use crate::{gl_ctx, log, rt, Launcher};


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
    launcher: Launcher,
    /// The map selector while no map has been chosen.
    menu: Option<Menu>,
    direct: Option<MapInfo>,
    cursor: (f64, f64),
    #[cfg(target_os = "ios")]
    menu_overlay: Option<crate::menu::Overlay>,
    #[cfg(not(target_os = "ios"))]
    menu_art: Option<crate::menu::GlArt>,
    window: Option<Window>,
    booted: bool,
    pads: Option<Pads>,
    fake_pad: Option<FakePad>,
    mapper: Mapper,
    grabbed: bool,
    /// The mouse was released because a game menu took the cursor; re-lock when it closes.
    regrab: bool,
    started: Instant,
    autoplay: Option<f32>,
    #[cfg(target_os = "ios")]
    loading_overlay: Option<crate::loading::Overlay>,
}

impl App {
    pub fn new(egl_lib: PathBuf, launcher: Launcher, menu: Menu, direct: Option<MapInfo>) -> Self {
        Self {
            egl_lib,
            launcher,
            menu: Some(menu),
            direct,
            cursor: (0.0, 0.0),
            #[cfg(target_os = "ios")]
            menu_overlay: None,
            #[cfg(not(target_os = "ios"))]
            menu_art: None,
            window: None,
            booted: false,
            pads: None,
            fake_pad: FakePad::from_env(),
            mapper: Mapper::new(),
            grabbed: false,
            regrab: false,
            started: Instant::now(),
            // Debug: BO1Z_AUTOPLAY=<seconds> releases the intro gate without input.
            autoplay: std::env::var("BO1Z_AUTOPLAY").ok().and_then(|s| s.parse().ok()),
            #[cfg(target_os = "ios")]
            loading_overlay: None,
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
        if let Some(m) = self.direct.take() {
            return self.launch(m);
        }
        #[cfg(target_os = "ios")]
        if let (Some(v), Some(menu)) = (self.window.as_ref().and_then(game_view), self.menu.as_ref()) {
            self.menu_overlay = crate::menu::Overlay::new(&v, menu);
        }
        self.draw_menu();
        Ok(())
    }

    /// Boot the engine with the chosen map, behind the loading bar.
    fn launch(&mut self, m: MapInfo) -> anyhow::Result<()> {
        self.menu = None;
        #[cfg(target_os = "ios")]
        {
            if let Some(o) = self.menu_overlay.take() {
                o.remove();
            }
            self.loading_overlay = self.window.as_ref().and_then(game_view).and_then(|v| crate::loading::Overlay::new(&v));
        }
        #[cfg(not(target_os = "ios"))]
        {
            if let Some(a) = self.menu_art.take() {
                if gl_ctx::make_current().is_ok() {
                    a.delete();
                }
            }
            gl_ctx::release_current();
            if let Some(w) = self.window.as_ref() {
                w.set_title(&format!("BO1Z - {}", m.name));
            }
        }
        // Render at the drawing-buffer size (engine.js passes the canvas size the same way).
        let boot = self.launcher.boot(&m, gl_ctx::surface_size())?;
        rt::start_browser_main(boot);
        self.booted = true;
        Ok(())
    }

    fn choose(&mut self, i: usize) {
        let Some(m) = self.menu.as_ref().and_then(|menu| menu.maps.get(i).cloned()) else { return };
        log::info(&format!("selected {}", m.name));
        if let Err(e) = self.launch(m) {
            fatal(&format!("cannot start map: {e:?}"));
        }
    }

    /// Redraw the selector if its state changed.
    fn draw_menu(&mut self) {
        let Some(menu) = self.menu.as_mut() else { return };
        if !menu.dirty {
            return;
        }
        menu.dirty = false;
        #[cfg(target_os = "ios")]
        if let Some(o) = self.menu_overlay.as_ref() {
            o.update(menu);
        }
        #[cfg(not(target_os = "ios"))]
        {
            let (w, h) = gl_ctx::surface_size();
            if gl_ctx::make_current().is_ok() {
                let art = self.menu_art.get_or_insert_with(crate::menu::GlArt::new);
                crate::menu::draw_gl(menu, art, w, h);
                gl_ctx::swap();
            }
            if let Some(win) = self.window.as_ref() {
                if let Some(m) = menu.maps.get(menu.sel) {
                    win.set_title(&format!("BO1Z - {}  (arrows / d-pad, Enter / A to play)", m.name));
                }
            }
        }
    }

    /// Window size in physical pixels (the unit of cursor and touch positions).
    fn win_size(&self) -> (f32, f32) {
        self.window.as_ref().map_or((1.0, 1.0), |w| {
            let s = w.inner_size();
            (s.width.max(1) as f32, s.height.max(1) as f32)
        })
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
        if self.menu.is_some() {
            let pad = match (self.fake_pad.as_mut(), self.pads.as_mut()) {
                (Some(f), _) => Some(f.poll(self.started.elapsed().as_secs_f32())),
                (None, Some(p)) => Some(p.poll()),
                (None, None) => None,
            };
            if let Some(pad) = pad {
                let chosen = self.menu.as_mut().and_then(|m| m.pad(&pad));
                if let Some(i) = chosen {
                    return self.choose(i);
                }
            }
            // Debug: BO1Z_AUTOPICK=<slug> chooses without input.
            if let Ok(slug) = std::env::var("BO1Z_AUTOPICK") {
                if let Some(i) = self.menu.as_ref().and_then(|m| m.maps.iter().position(|x| x.slug == slug)) {
                    return self.choose(i);
                }
            }
            self.draw_menu();
            return;
        }
        #[cfg(target_os = "ios")]
        if self.loading_overlay.as_ref().is_some_and(|o| !o.update()) {
            self.loading_overlay = None;
        }
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
        let pad = match (self.fake_pad.as_mut(), self.pads.as_mut()) {
            (Some(f), _) => f.poll(self.started.elapsed().as_secs_f32()),
            (None, Some(p)) => p.poll(),
            (None, None) => return,
        };
        let menu = rt::ui_state() & 1 != 0;
        // Desktop: like the browser, a game menu (pause, settings) gets an absolute cursor;
        // gameplay gets the locked, relative mouse back when the menu closes.
        #[cfg(not(target_os = "ios"))]
        if menu && self.grabbed {
            self.set_grab(false);
            self.regrab = true;
            if let Some(w) = self.window.as_ref() {
                let s = w.inner_size();
                let c = winit::dpi::PhysicalPosition::new(s.width as f64 / 2.0, s.height as f64 / 2.0);
                let _ = w.set_cursor_position(c);
                self.cursor = (c.x, c.y);
                rt::post_input(input::CURSOR, c.x as i32, c.y as i32);
            }
        } else if !menu && self.regrab {
            self.regrab = false;
            if self.window.as_ref().is_some_and(|w| w.has_focus()) {
                self.set_grab(true);
            }
        }
        for ev in self.mapper.update(&pad, menu) {
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
            WindowEvent::Resized(_) if self.menu.is_some() => {
                if let Some(m) = self.menu.as_mut() {
                    m.dirty = true;
                }
                self.draw_menu();
            }
            WindowEvent::KeyboardInput { event, .. } if self.menu.is_some() => {
                if let (PhysicalKey::Code(code), ElementState::Pressed) = (event.physical_key, event.state) {
                    if let Some(i) = self.menu.as_mut().and_then(|m| m.key(code)) {
                        self.choose(i);
                    } else {
                        self.draw_menu();
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } if self.menu.is_some() => {
                self.cursor = (position.x, position.y);
                let (w, h) = self.win_size();
                if let Some(m) = self.menu.as_mut() {
                    m.hover(position.x as f32, position.y as f32, w, h);
                }
                self.draw_menu();
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } if self.menu.is_some() => {
                let ((x, y), (w, h)) = ((self.cursor.0 as f32, self.cursor.1 as f32), self.win_size());
                if let Some(i) = self.menu.as_mut().and_then(|m| m.tap(x, y, w, h)) {
                    self.choose(i);
                }
            }
            WindowEvent::Touch(t) if self.menu.is_some() => {
                if t.phase == winit::event::TouchPhase::Ended {
                    let ((x, y), (w, h)) = ((t.location.x as f32, t.location.y as f32), self.win_size());
                    if let Some(i) = self.menu.as_mut().and_then(|m| m.tap(x, y, w, h)) {
                        self.choose(i);
                    }
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
                // An unlocked click in gameplay only re-captures the mouse; in a menu it clicks.
                let in_menu = rt::ready() && rt::ui_state() & 1 != 0;
                if !self.grabbed && !in_menu && state == ElementState::Pressed && button == MouseButton::Left {
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
                self.cursor = (position.x, position.y);
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

/// The winit root view the game renders into (the loading bar is added on top of it).
#[cfg(target_os = "ios")]
fn game_view(w: &Window) -> Option<objc2::rc::Retained<objc2_ui_kit::UIView>> {
    use objc2::Message;
    let RawWindowHandle::UiKit(h) = w.window_handle().ok()?.as_raw() else { return None };
    let view: &objc2_ui_kit::UIView = unsafe { h.ui_view.cast::<objc2_ui_kit::UIView>().as_ref() };
    Some(view.retain())
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
        // 1x rendering: Retina (2x) would quadruple the pixels this engine has to shade.
        view.setContentScaleFactor(1.0);
        let layer = view.layer();
        layer.setContentsScale(1.0);
        Ok(Retained::into_raw(layer) as *mut std::ffi::c_void)
    }
}
