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


static BACK_TO_MENU: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask the UI thread to leave the game for the map select.
/// Debug: BO1Z_NOINPUT=1 ignores the real mouse and keyboard and never captures the cursor,
/// for scripted runs (BO1Z_FAKEPAD) on a desktop someone is using.
fn no_real_input() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BO1Z_NOINPUT").is_some())
}

pub fn return_to_menu() {
    if !BACK_TO_MENU.swap(true, std::sync::atomic::Ordering::Relaxed) {
        log::info("game quit: returning to the map select");
    }
}

/// The engine cannot be torn down and rebooted in place (its threads and 2 GiB heap stay
/// live), so returning to the map select restarts the app. Desktop relaunches itself with
/// the same arguments minus `--map`; iOS cannot spawn processes, so the app exits and
/// opens on the map select next time.
fn restart_to_menu() -> ! {
    log::flush();
    #[cfg(not(target_os = "ios"))]
    {
        let mut args: Vec<String> = std::env::args().skip(1).collect();
        if let Some(i) = args.iter().position(|a| a == "--map") {
            args.drain(i..(i + 2).min(args.len()));
        }
        if let Ok(exe) = std::env::current_exe() {
            match std::process::Command::new(exe).args(&args).spawn() {
                Ok(_) => log::info("relaunched into the map select"),
                Err(e) => log::error(&format!("relaunch failed: {e}")),
            }
        }
    }
    quit(0)
}

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
    /// Desktop loading cinematic: a child window over the game, and its player.
    #[cfg(windows)]
    load_video: Option<(Window, crate::loadvideo::LoadVideo)>,
    /// The active event loop, valid only while a handler runs (window creation needs it).
    event_loop: Option<*const ActiveEventLoop>,

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
    video_overlay: Option<crate::loading::VideoOverlay>,
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
            #[cfg(windows)]
            load_video: None,
            event_loop: None,

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
            video_overlay: None,
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

    /// Boot the engine with the chosen map, with its loading cinematic playing on top.
    /// Order matters: the map is set up first, then the video starts, then the engine.
    fn launch(&mut self, m: MapInfo) -> anyhow::Result<()> {
        // Render at the drawing-buffer size (engine.js passes the canvas size the same way).
        let boot = self.launcher.boot(&m, gl_ctx::surface_size())?;
        let video = self.launcher.menu_dir().join(format!("load-{}.mp4", m.slug));
        crate::loading::set_video(video.is_file().then_some(video));
        #[cfg(target_os = "ios")]
        {
            if let Some(o) = self.menu_overlay.take() {
                o.remove();
            }
            self.video_overlay = match (crate::loading::video(), self.window.as_ref().and_then(game_view)) {
                (Some(v), Some(view)) => crate::loading::VideoOverlay::new(&view, &v),
                _ => None,
            };
        }
        #[cfg(not(target_os = "ios"))]
        {
            // The menu drew on the UI thread; the engine thread takes the context now.
            if gl_ctx::make_current().is_ok() {
                unsafe {
                    gl::ClearColor(0.0, 0.0, 0.0, 1.0);
                    gl::Clear(gl::COLOR_BUFFER_BIT);
                }
                gl_ctx::swap();
            }
            gl_ctx::release_current();
            #[cfg(windows)]
            {
                self.load_video = self.start_video();
            }
            if let Some(w) = self.window.as_ref() {
                w.set_title(&format!("BO1Z - {}", m.name));
            }
        }
        self.menu = None;
        rt::start_browser_main(boot);
        self.booted = true;
        Ok(())
    }

    /// Desktop: the cinematic in a borderless child window covering the game window.
    #[cfg(windows)]
    fn start_video(&mut self) -> Option<(Window, crate::loadvideo::LoadVideo)> {
        let video = crate::loading::video()?;
        let el = self.event_loop.as_ref()?;
        let parent = self.window.as_ref()?;
        let size = parent.inner_size();
        let raw = parent.window_handle().ok()?.as_raw();
        let attrs = Window::default_attributes()
            .with_decorations(false)
            .with_inner_size(size)
            .with_position(winit::dpi::PhysicalPosition::new(0, 0));
        let attrs = unsafe { attrs.with_parent_window(Some(raw)) };
        // SAFETY: the pointer is only used during this call, inside the event loop.
        let el = unsafe { &**el };
        let child = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                log::error(&format!("video window: {e}"));
                return None;
            }
        };
        let native = native_window(&child).ok()?;
        match crate::loadvideo::LoadVideo::start(native, &video) {
            Ok(v) => Some((child, v)),
            Err(e) => {
                log::error(&format!("loading video: {e}"));
                None
            }
        }
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
                crate::menu::draw_gl(menu, w, h);
                gl_ctx::swap();
            }
            if let Some(win) = self.window.as_ref() {
                if let Some(m) = menu.maps.get(menu.sel) {
                    win.set_title(&format!("BO1Z - {}  (arrows / d-pad, Enter / A to play)", m.name));
                }
            }
        }
    }

    /// Remove the loading cinematic, if it is still up.
    fn end_video(&mut self) {
        #[cfg(windows)]
        if self.load_video.take().is_some() {
            // Dropping stops ffmpeg and destroys the child window.
            if let Some(w) = self.window.as_ref() {
                w.focus_window();
            }
        }
        #[cfg(target_os = "ios")]
        if let Some(o) = self.video_overlay.take() {
            o.remove();
        }
    }

    fn video_running(&self) -> bool {
        #[cfg(windows)]
        return self.load_video.is_some();
        #[cfg(target_os = "ios")]
        return self.video_overlay.is_some();
        #[cfg(not(any(windows, target_os = "ios")))]
        return false;
    }

    /// The map has loaded and the loading screen asks the player to start.
    fn waiting_to_start(&self) -> bool {
        self.booted && crate::loading::done() && !crate::loading::is_released()
    }

    fn start_game(&mut self) {
        self.end_video();
        rt::post_input(input::INTRO, 1, 0);
        #[cfg(not(target_os = "ios"))]
        if !self.grabbed {
            self.set_grab(true);
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
        if no_real_input() {
            self.grabbed = on;
            self.send((input::LOCK, on as i32, 0));
            return;
        }
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
        // The loading cinematic: removed when it ends or once the game has started.
        #[cfg(windows)]
        let ended = match self.load_video.as_mut() {
            Some((child, v)) => {
                if let Some(p) = self.window.as_ref().map(|w| w.inner_size()) {
                    if child.inner_size() != p {
                        let _ = child.request_inner_size(p);
                    }
                }
                v.tick() || crate::loading::is_released()
            }
            None => false,
        };
        #[cfg(target_os = "ios")]
        let ended = match self.video_overlay.as_mut() {
            Some(o) => o.tick() || crate::loading::is_released(),
            None => false,
        };
        #[cfg(not(any(windows, target_os = "ios")))]
        let ended = false;
        if ended {
            log::info("loading video finished");
            self.end_video();
        }
        // Like the site: once the map has loaded and the video is gone, the game starts.
        if self.waiting_to_start() && !self.video_running() {
            self.start_game();
        }
        if BACK_TO_MENU.load(std::sync::atomic::Ordering::Relaxed) {
            restart_to_menu();
        }
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
        if !self.booted || !rt::ready() {
            return;
        }
        if let Some(secs) = self.autoplay {
            if self.started.elapsed().as_secs_f32() > secs && crate::loading::done() {
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
                if !no_real_input() {
                    let _ = w.set_cursor_position(c);
                }
                self.cursor = (c.x, c.y);
                rt::post_input(input::CURSOR, c.x as i32, c.y as i32);
            }
        } else if !menu && self.regrab {
            self.regrab = false;
            if no_real_input() || self.window.as_ref().is_some_and(|w| w.has_focus()) {
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
        self.event_loop = Some(el as *const _);
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.start(el) {
            fatal(&format!("startup failed: {e:?}"));
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _ev: ()) {}

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, ev: WindowEvent) {
        self.event_loop = Some(el as *const _);
        if no_real_input()
            && matches!(
                ev,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::CursorMoved { .. }
                    | WindowEvent::Touch(_)
                    | WindowEvent::Focused(_)
            )
        {
            return;
        }
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
                // Enter / Space on the "press to start" screen.
                if event.state == ElementState::Pressed && !event.repeat && self.waiting_to_start() {
                    use winit::keyboard::KeyCode as K;
                    if matches!(event.physical_key, PhysicalKey::Code(K::Enter | K::NumpadEnter | K::Space)) {
                        self.start_game();
                        return;
                    }
                }
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
                if state == ElementState::Pressed && button == MouseButton::Left && self.waiting_to_start() {
                    self.start_game();
                    return;
                }
                // An unlocked click in gameplay only re-captures the mouse; in a menu it clicks.
                let in_menu = rt::ready() && rt::ui_state() & 1 != 0;
                if !self.grabbed && !in_menu && state == ElementState::Pressed && button == MouseButton::Left {
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
        if no_real_input() {
            return;
        }
        if let DeviceEvent::MouseMotion { delta } = ev {
            if self.grabbed {
                self.send((input::MOUSE, delta.0 as i32, delta.1 as i32));
            }
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.event_loop = Some(el as *const _);
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
