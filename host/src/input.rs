//! Controller -> the engine's own gamepad support.
//!
//! The engine still carries BO1's console gamepad code (GPad_*, CL_GamepadAxisValue,
//! bindaxis, BUTTON_* keys, button-glyph hints, the BrowserFrame menu cursor); only the
//! XInput poller that fed it was left out of the web build. The host takes its place:
//! every tick it writes pad 0's GamePad struct and the six physical axes, and sends
//! BUTTON_*/DPAD_* key events so the binds from default_controller.cfg apply.
//!
//! Addresses are from KisakBlack-native.wasm (see GPad_GetStick, GPad_GetButton,
//! CL_GamepadAxisValue) and must be re-derived if the engine binary changes.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::mem;

/// KB_Input kinds (engine.js).
pub const KEY: i32 = 0;
pub const MOUSE: i32 = 2;
pub const LOCK: i32 = 3;
pub const CURSOR: i32 = 4;
pub const INTRO: i32 = 7;

/// Engine key numbers (keynames table).
pub mod key {
    pub const ENTER: i32 = 13;
    pub const ESCAPE: i32 = 27;
    pub const UPARROW: i32 = 154;
    pub const DOWNARROW: i32 = 155;
    pub const LEFTARROW: i32 = 156;
    pub const RIGHTARROW: i32 = 157;
    pub const BUTTON_A: i32 = 1;
    pub const BUTTON_B: i32 = 2;
    pub const BUTTON_X: i32 = 3;
    pub const BUTTON_Y: i32 = 4;
    pub const BUTTON_LSHLDR: i32 = 5;
    pub const BUTTON_RSHLDR: i32 = 6;
    pub const BUTTON_START: i32 = 14;
    pub const BUTTON_BACK: i32 = 15;
    pub const BUTTON_LSTICK: i32 = 16;
    pub const BUTTON_RSTICK: i32 = 17;
    pub const BUTTON_LTRIG: i32 = 18;
    pub const BUTTON_RTRIG: i32 = 19;
    pub const DPAD_UP: i32 = 20;
    pub const DPAD_DOWN: i32 = 21;
    pub const DPAD_LEFT: i32 = 22;
    pub const DPAD_RIGHT: i32 = 23;
}

/// GamePad structs (124 bytes each, pad 0 first):
///   +0 u8 enabled, +2 i16 digitals, +4 i16 lastDigitals,
///   +8 f32 analogs[2] (LT, RT), +16 lastAnalogs[2], +24 f32 sticks[4] (LX, LY, RX, RY),
///   +40 lastSticks[4].
const GPAD0: u32 = 147_054_240;
/// Physical axis values read by CL_GamepadAxisValue: i32[6] scaled by 1/65535, in the
/// order of the engine's axis names: A_RSTICK_X, A_RSTICK_Y, A_LSTICK_X, A_LSTICK_Y,
/// A_RTRIGGER, A_LTRIGGER.
const PHYS_AXES: u32 = 13_382_032;

/// XInput-style digital bits (GamePadButton & 0xffff).
mod bit {
    pub const UP: u16 = 0x0001;
    pub const DOWN: u16 = 0x0002;
    pub const LEFT: u16 = 0x0004;
    pub const RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const L3: u16 = 0x0040;
    pub const R3: u16 = 0x0080;
    pub const LB: u16 = 0x0100;
    pub const RB: u16 = 0x0200;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// Startup commands: load BO1's controller layout (Start = pause menu, Back = scoreboard,
/// sticks via bindaxis). gpad_enabled is driven by `set_gpad_dvars` instead, so keyboard
/// play keeps keyboard hint text.
pub fn startup_args() -> Vec<String> {
    ["+exec", "default_controller.cfg"].iter().map(|s| s.to_string()).collect()
}

/// Pointers to the gpad_enabled / gpad_present dvars (GPad_InitAll stores them here).
const DVAR_GPAD_ENABLED: u32 = 147_054_220;
const DVAR_GPAD_PRESENT: u32 = 147_054_224;
/// dvar_s.current for a bool dvar (CL_CreateCmd reads gpad_enabled at this offset).
const DVAR_CURRENT: u32 = 24;

/// gpad_enabled/gpad_present follow the controller: on gives button-glyph hints and the
/// gamepad view code; off gives keyboard hints.
fn set_gpad_dvars(on: bool) {
    for slot in [DVAR_GPAD_ENABLED, DVAR_GPAD_PRESENT] {
        let dvar = mem::u32(slot);
        if dvar != 0 && mem::u8(dvar + DVAR_CURRENT) != on as u8 {
            mem::set_u8(dvar + DVAR_CURRENT, on as u8);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Btn {
    A,
    B,
    X,
    Y,
    LB,
    RB,
    L3,
    R3,
    Start,
    Back,
    Up,
    Down,
    Left,
    Right,
}

impl Btn {
    const ALL: [Btn; 14] = [
        Btn::A, Btn::B, Btn::X, Btn::Y, Btn::LB, Btn::RB, Btn::L3, Btn::R3,
        Btn::Start, Btn::Back, Btn::Up, Btn::Down, Btn::Left, Btn::Right,
    ];

    fn bit(self) -> u16 {
        match self {
            Btn::A => bit::A,
            Btn::B => bit::B,
            Btn::X => bit::X,
            Btn::Y => bit::Y,
            Btn::LB => bit::LB,
            Btn::RB => bit::RB,
            Btn::L3 => bit::L3,
            Btn::R3 => bit::R3,
            Btn::Start => bit::START,
            Btn::Back => bit::BACK,
            Btn::Up => bit::UP,
            Btn::Down => bit::DOWN,
            Btn::Left => bit::LEFT,
            Btn::Right => bit::RIGHT,
        }
    }

    fn key(self) -> i32 {
        match self {
            Btn::A => key::BUTTON_A,
            Btn::B => key::BUTTON_B,
            Btn::X => key::BUTTON_X,
            Btn::Y => key::BUTTON_Y,
            Btn::LB => key::BUTTON_LSHLDR,
            Btn::RB => key::BUTTON_RSHLDR,
            Btn::L3 => key::BUTTON_LSTICK,
            Btn::R3 => key::BUTTON_RSTICK,
            Btn::Start => key::BUTTON_START,
            Btn::Back => key::BUTTON_BACK,
            Btn::Up => key::DPAD_UP,
            Btn::Down => key::DPAD_DOWN,
            Btn::Left => key::DPAD_LEFT,
            Btn::Right => key::DPAD_RIGHT,
        }
    }
}

/// Controller family, for logging and (later) glyph selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PadKind {
    #[default]
    Xbox,
    PlayStation,
    Other,
}

#[derive(Clone, Default, Debug)]
pub struct PadState {
    pub connected: bool,
    pub kind: PadKind,
    /// Sticks in [-1, 1], y positive = up.
    pub lx: f32,
    pub ly: f32,
    pub rx: f32,
    pub ry: f32,
    /// Triggers in [0, 1].
    pub lt: f32,
    pub rt: f32,
    pub buttons: HashSet<Btn>,
}

pub type Event = (i32, i32, i32);

/// Trigger travel that counts as a BUTTON_LTRIG/RTRIG press (gpad_button_deadzone-like).
const TRIGGER_PRESS: f32 = 0.3;
/// Stick deflection that counts as a menu direction, and where it lets go again.
const STICK_PRESS: f32 = 0.5;
const STICK_RELEASE: f32 = 0.35;
/// Held menu directions repeat like a keyboard: first after this delay, then at this rate.
const REPEAT_FIRST: Duration = Duration::from_millis(400);
const REPEAT_EVERY: Duration = Duration::from_millis(130);

pub struct Mapper {
    held: HashSet<i32>,
    /// Buttons held through a menu opening or closing: ignored until released, so the press
    /// that changed the menu does not act again in the new context.
    stale: HashSet<Btn>,
    /// Menu direction keys held, with the time of their next repeat.
    repeat: Vec<(i32, Instant)>,
    stick_dir: [bool; 4],
    menu: Option<bool>,
    intro_released: bool,
    intro_armed: bool,
    was_connected: bool,
}

impl Mapper {
    pub fn new() -> Self {
        Self {
            held: HashSet::new(),
            stale: HashSet::new(),
            repeat: Vec::new(),
            stick_dir: [false; 4],
            menu: None,
            intro_released: false,
            intro_armed: false,
            was_connected: false,
        }
    }

    /// A menu direction: pressed once, then repeated while held.
    fn nav(&mut self, out: &mut Vec<Event>, k: i32, down: bool) {
        let now = Instant::now();
        if down && !self.held.contains(&k) {
            self.repeat.push((k, now + REPEAT_FIRST));
        } else if down {
            if let Some(r) = self.repeat.iter_mut().find(|r| r.0 == k) {
                if now >= r.1 {
                    r.1 = now + REPEAT_EVERY;
                    out.push((KEY, k, 1));
                }
            }
        } else {
            self.repeat.retain(|r| r.0 != k);
        }
        self.set(out, k, down);
    }

    /// Left stick as four menu directions (up, down, left, right), with hysteresis.
    fn stick_dirs(&mut self, pad: &PadState) -> [bool; 4] {
        let axes = [pad.ly, -pad.ly, -pad.lx, pad.lx];
        for (on, v) in self.stick_dir.iter_mut().zip(axes) {
            *on = if *on { v > STICK_RELEASE } else { v > STICK_PRESS };
        }
        self.stick_dir
    }

    fn set(&mut self, out: &mut Vec<Event>, k: i32, down: bool) {
        if down && self.held.insert(k) {
            out.push((KEY, k, 1));
        } else if !down && self.held.remove(&k) {
            out.push((KEY, k, 0));
        }
    }

    fn release_all(&mut self, out: &mut Vec<Event>) {
        let mut keys: Vec<i32> = self.held.drain().collect();
        keys.sort();
        for k in keys {
            out.push((KEY, k, 0));
        }
    }

    /// One tick: mirror the pad into engine memory and return key events for KB_Input.
    pub fn update(&mut self, pad: &PadState, menu: bool) -> Vec<Event> {
        let mut out = Vec::new();
        // Every tick: the dvars are registered after startup and profile loads reset them.
        set_gpad_dvars(pad.connected);
        if !pad.connected {
            if self.was_connected {
                self.release_all(&mut out);
                write_gpad(&PadState::default());
                self.was_connected = false;
            }
            return out;
        }
        if !self.was_connected {
            crate::log::info(&format!("controller active ({:?})", pad.kind));
            self.was_connected = true;
        }
        write_gpad(pad);

        // The engine holds the first loaded frame until the player is "in" (engine.js play()).
        // Only after loading, and only on a fresh press (not the A that picked the map).
        let start_held = pad.buttons.contains(&Btn::A) || pad.buttons.contains(&Btn::Start);
        if !self.intro_released && crate::loading::done() {
            if !start_held {
                self.intro_armed = true;
            } else if self.intro_armed {
                self.intro_released = true;
                out.push((INTRO, 1, 0));
            }
        }

        // Menus unlock the pointer.
        if self.menu != Some(menu) {
            self.release_all(&mut out);
            self.repeat.clear();
            self.stale = pad.buttons.clone();
            out.push((LOCK, (!menu) as i32, 0));
            self.menu = Some(menu);
        }
        self.stale.retain(|b| pad.buttons.contains(b));
        let active: HashSet<Btn> = pad.buttons.difference(&self.stale).copied().collect();
        let on = |b: Btn| active.contains(&b);

        if menu {
            // The engine's menus take arrows, Enter and Escape; the pad's DPAD_*/BUTTON_*
            // keys only reach their gameplay binds there (DPAD_DOWN = +actionslot 2).
            let [s_up, s_down, s_left, s_right] = self.stick_dirs(pad);
            let (up, down, left, right) = (on(Btn::Up) || s_up, on(Btn::Down) || s_down, on(Btn::Left) || s_left, on(Btn::Right) || s_right);
            let (a, b, start, back) = (on(Btn::A), on(Btn::B), on(Btn::Start), on(Btn::Back));
            self.nav(&mut out, key::UPARROW, up);
            self.nav(&mut out, key::DOWNARROW, down);
            self.nav(&mut out, key::LEFTARROW, left);
            self.nav(&mut out, key::RIGHTARROW, right);
            self.set(&mut out, key::ENTER, a);
            self.set(&mut out, key::ESCAPE, b);
            // Start closes the pause menu (togglemenu), as on console.
            self.set(&mut out, key::BUTTON_START, start);
            self.set(&mut out, key::BUTTON_BACK, back);
            return out;
        }

        for b in Btn::ALL {
            self.set(&mut out, b.key(), on(b));
        }
        self.set(&mut out, key::BUTTON_LTRIG, pad.lt > TRIGGER_PRESS);
        self.set(&mut out, key::BUTTON_RTRIG, pad.rt > TRIGGER_PRESS);
        out
    }
}

/// Write pad 0's GamePad struct and the physical axes, as GPad_UpdateAll did on Xbox.
fn write_gpad(pad: &PadState) {
    let digitals = Btn::ALL.iter().filter(|b| pad.buttons.contains(b)).fold(0u16, |m, b| m | b.bit());
    let g = GPAD0;
    // Previous state first, so GPad_IsButtonPressed sees the edge.
    mem::write(g + 4, &mem::bytes(g + 2, 2).to_vec());
    mem::write(g + 16, &mem::bytes(g + 8, 8).to_vec());
    mem::write(g + 40, &mem::bytes(g + 24, 16).to_vec());
    mem::set_u8(g, pad.connected as u8);
    mem::write(g + 2, &digitals.to_le_bytes());
    // The engine asserts every axis deflection stays within [-1, 1] (cl_gamepad.cpp:744,
    // CL_GamepadAxisValue). XInput sticks report inside a circle, but iOS GameController
    // reports them on a square: a full diagonal is (1, 1), length 1.41, which the engine's
    // radial deadzone maps past 1 and aborts on. Clamp each stick to the unit circle (a
    // hair inside, for float slack) and each trigger to [0, 1].
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let stick = |x: f32, y: f32| {
        let (x, y) = (finite(x).clamp(-1.0, 1.0), finite(y).clamp(-1.0, 1.0));
        let len = (x * x + y * y).sqrt();
        const MAX: f32 = 0.999;
        if len > MAX { (x * MAX / len, y * MAX / len) } else { (x, y) }
    };
    let trigger = |v: f32| finite(v).clamp(0.0, 1.0);
    let (lx, ly) = stick(pad.lx, pad.ly);
    let (rx, ry) = stick(pad.rx, pad.ry);
    let (lt, rt) = (trigger(pad.lt), trigger(pad.rt));
    mem::set_f32(g + 8, lt);
    mem::set_f32(g + 12, rt);
    for (i, v) in [lx, ly, rx, ry].into_iter().enumerate() {
        mem::set_f32(g + 24 + 4 * i as u32, v);
    }
    let axis = |v: f32| (v.clamp(-1.0, 1.0) * 65535.0) as i32;
    for (i, v) in [rx, ry, lx, ly, rt, lt].into_iter().enumerate() {
        mem::set_i32(PHYS_AXES + 4 * i as u32, axis(v));
    }
}

// ---------------------------------------------------------------------------------------
// Debug: BO1Z_FAKEPAD scripts a virtual controller on desktop, for testing without one.
// Format: "<seconds>:<input>[=<value>];..." where input is a Btn name (A, Start, Down, ...)
// or an axis (lx, ly, rx, ry, lt, rt). A button without "=0" is pressed; "=0" releases it.
// "shot" saves a screenshot of the next frame, "quit" saves the shader list and exits.
// A time written "+<seconds>" counts from the game start (intro released) instead of launch.
// Example: BO1Z_FAKEPAD="20:Start;20.2:Start=0;22:Down;22.2:Down=0;30:ly=1;32:ly=0"

pub struct FakePad {
    /// (time, relative to the game start?, input, value), in order.
    steps: Vec<(f32, bool, String, f32)>,
    state: PadState,
    started: Option<std::time::Instant>,
}

impl FakePad {
    pub fn from_env() -> Option<Self> {
        let spec = std::env::var("BO1Z_FAKEPAD").ok()?;
        let mut steps = Vec::new();
        for item in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
            let (t, rest) = item.split_once(':')?;
            let (name, v) = rest.split_once('=').map_or((rest, 1.0), |(n, v)| (n, v.parse().unwrap_or(0.0)));
            let (rel, t) = t.strip_prefix('+').map_or((false, t), |t| (true, t));
            steps.push((t.parse::<f32>().ok()?, rel, name.to_string(), v));
        }
        steps.sort_by(|a, b| (a.1, a.0).partial_cmp(&(b.1, b.0)).unwrap_or(std::cmp::Ordering::Equal));
        crate::log::info(&format!("fake pad: {} steps", steps.len()));
        Some(Self { steps, state: PadState { connected: true, ..Default::default() }, started: None })
    }

    /// `t`: seconds since launch. Steps written "+<t>" count from the game start instead.
    pub fn poll(&mut self, t: f32) -> PadState {
        if self.started.is_none() && crate::loading::is_released() {
            self.started = Some(std::time::Instant::now());
        }
        let game_t = self.started.map(|s| s.elapsed().as_secs_f32());
        while let Some(i) = self.steps.iter().position(|s| if s.1 { game_t.is_some_and(|g| s.0 <= g) } else { s.0 <= t }) {
            let (_, _, name, v) = self.steps.remove(i);
            crate::log::info(&format!("fake pad @{t:.1}s: {name}={v}"));
            let s = &mut self.state;
            match name.as_str() {
                "lx" => s.lx = v,
                "ly" => s.ly = v,
                "rx" => s.rx = v,
                "ry" => s.ry = v,
                "lt" => s.lt = v,
                "rt" => s.rt = v,
                "shot" => crate::gl_ctx::request_shot(),
                "quit" => {
                    crate::shadercache::save_now();
                    crate::platform::quit(0);
                }
                b => {
                    if let Some(btn) = Btn::ALL.iter().find(|x| format!("{x:?}").eq_ignore_ascii_case(b)) {
                        if v != 0.0 {
                            s.buttons.insert(*btn);
                        } else {
                            s.buttons.remove(btn);
                        }
                    }
                }
            }
        }
        self.state.clone()
    }
}

// ---------------------------------------------------------------------------------------
// Pad backends

/// Controller family from its name / GCController.productCategory.
fn pad_kind(name: &str) -> PadKind {
    let n = name.to_ascii_lowercase();
    if ["dualsense", "dualshock", "playstation", "ps4", "ps5", "wireless controller"].iter().any(|k| n.contains(k)) {
        PadKind::PlayStation
    } else if n.contains("xbox") || n.contains("xinput") {
        PadKind::Xbox
    } else {
        PadKind::Other
    }
}

#[cfg(not(target_os = "ios"))]
pub struct Pads {
    g: Option<gilrs::Gilrs>,
}

#[cfg(not(target_os = "ios"))]
impl Pads {
    pub fn new() -> Self {
        match gilrs::Gilrs::new() {
            Ok(g) => {
                for (_, p) in g.gamepads() {
                    crate::log::info(&format!("controller: {}", p.name()));
                }
                Self { g: Some(g) }
            }
            Err(e) => {
                crate::log::error(&format!("controller support unavailable: {e}"));
                Self { g: None }
            }
        }
    }

    pub fn poll(&mut self) -> PadState {
        use gilrs::{Axis, Button};
        let Some(g) = self.g.as_mut() else { return PadState::default() };
        while let Some(ev) = g.next_event() {
            if let gilrs::EventType::Connected = ev.event {
                crate::log::info(&format!("controller connected: {}", g.gamepad(ev.id).name()));
            }
        }
        let Some((_, p)) = g.gamepads().find(|(_, p)| p.is_connected()) else { return PadState::default() };
        let mut s = PadState {
            connected: true,
            kind: pad_kind(p.name()),
            lx: p.value(Axis::LeftStickX),
            ly: p.value(Axis::LeftStickY),
            rx: p.value(Axis::RightStickX),
            ry: p.value(Axis::RightStickY),
            lt: p.button_data(Button::LeftTrigger2).map_or(0.0, |d| d.value()),
            rt: p.button_data(Button::RightTrigger2).map_or(0.0, |d| d.value()),
            buttons: HashSet::new(),
        };
        for (gb, b) in [
            (Button::South, Btn::A),
            (Button::East, Btn::B),
            (Button::West, Btn::X),
            (Button::North, Btn::Y),
            (Button::LeftTrigger, Btn::LB),
            (Button::RightTrigger, Btn::RB),
            (Button::LeftThumb, Btn::L3),
            (Button::RightThumb, Btn::R3),
            (Button::Start, Btn::Start),
            (Button::Select, Btn::Back),
            (Button::DPadUp, Btn::Up),
            (Button::DPadDown, Btn::Down),
            (Button::DPadLeft, Btn::Left),
            (Button::DPadRight, Btn::Right),
        ] {
            if p.is_pressed(gb) {
                s.buttons.insert(b);
            }
        }
        s
    }
}

#[cfg(target_os = "ios")]
pub struct Pads;

#[cfg(target_os = "ios")]
impl Pads {
    pub fn new() -> Self {
        Self
    }

    /// GCController.controllers().first().extendedGamepad (Xbox One/Series pads over
    /// Bluetooth; Xbox 360 pads are not supported by iOS itself).
    pub fn poll(&mut self) -> PadState {
        use objc2_game_controller::{GCController, GCDevice};
        unsafe {
            let list = GCController::controllers();
            let Some(c) = list.firstObject() else { return PadState::default() };
            let Some(gp) = c.extendedGamepad() else { return PadState::default() };
            let mut s = PadState {
                connected: true,
                kind: pad_kind(&c.productCategory().to_string()),
                lx: gp.leftThumbstick().xAxis().value(),
                ly: gp.leftThumbstick().yAxis().value(),
                rx: gp.rightThumbstick().xAxis().value(),
                ry: gp.rightThumbstick().yAxis().value(),
                lt: gp.leftTrigger().value(),
                rt: gp.rightTrigger().value(),
                buttons: HashSet::new(),
            };
            let mut put = |on: bool, b: Btn| {
                if on {
                    s.buttons.insert(b);
                }
            };
            put(gp.buttonA().isPressed(), Btn::A);
            put(gp.buttonB().isPressed(), Btn::B);
            put(gp.buttonX().isPressed(), Btn::X);
            put(gp.buttonY().isPressed(), Btn::Y);
            put(gp.leftShoulder().isPressed(), Btn::LB);
            put(gp.rightShoulder().isPressed(), Btn::RB);
            put(gp.leftThumbstickButton().map_or(false, |b| b.isPressed()), Btn::L3);
            put(gp.rightThumbstickButton().map_or(false, |b| b.isPressed()), Btn::R3);
            put(gp.buttonMenu().isPressed(), Btn::Start);
            put(gp.buttonOptions().map_or(false, |b| b.isPressed()), Btn::Back);
            put(gp.dpad().up().isPressed(), Btn::Up);
            put(gp.dpad().down().isPressed(), Btn::Down);
            put(gp.dpad().left().isPressed(), Btn::Left);
            put(gp.dpad().right().isPressed(), Btn::Right);
            s
        }
    }
}

// ---------------------------------------------------------------------------------------
// Desktop keyboard (development only): physical key -> engine key code (input.js).

pub fn winit_key(code: winit::keyboard::KeyCode) -> Option<i32> {
    use winit::keyboard::KeyCode as K;
    Some(match code {
        K::Backquote => 126,
        K::Backspace => 127,
        K::Tab => 9,
        K::Enter => 13,
        K::Escape => 27,
        K::Space => 32,
        K::ShiftLeft | K::ShiftRight => 160,
        K::ControlLeft | K::ControlRight => 159,
        K::AltLeft | K::AltRight => 158,
        K::ArrowUp => 154,
        K::ArrowDown => 155,
        K::ArrowLeft => 156,
        K::ArrowRight => 157,
        K::Semicolon => 59,
        K::Equal => 61,
        K::Comma => 44,
        K::Minus => 45,
        K::Period => 46,
        K::Slash => 47,
        K::BracketLeft => 91,
        K::Backslash => 92,
        K::BracketRight => 93,
        K::Quote => 39,
        K::KeyA => 97, K::KeyB => 98, K::KeyC => 99, K::KeyD => 100, K::KeyE => 101, K::KeyF => 102,
        K::KeyG => 103, K::KeyH => 104, K::KeyI => 105, K::KeyJ => 106, K::KeyK => 107, K::KeyL => 108,
        K::KeyM => 109, K::KeyN => 110, K::KeyO => 111, K::KeyP => 112, K::KeyQ => 113, K::KeyR => 114,
        K::KeyS => 115, K::KeyT => 116, K::KeyU => 117, K::KeyV => 118, K::KeyW => 119, K::KeyX => 120,
        K::KeyY => 121, K::KeyZ => 122,
        K::Digit0 => 48, K::Digit1 => 49, K::Digit2 => 50, K::Digit3 => 51, K::Digit4 => 52,
        K::Digit5 => 53, K::Digit6 => 54, K::Digit7 => 55, K::Digit8 => 56, K::Digit9 => 57,
        K::F1 => 167, K::F2 => 168, K::F3 => 169, K::F4 => 170, K::F5 => 171, K::F6 => 172,
        K::F7 => 173, K::F8 => 174, K::F9 => 175, K::F10 => 176, K::F11 => 177, K::F12 => 178,
        _ => return None,
    })
}
