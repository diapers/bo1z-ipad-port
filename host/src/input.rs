//! Xbox 360/One controller -> the engine's keyboard/mouse input (KB_Input).
//!
//! The web build only accepts key/mouse events, so the pad drives the engine's built-in
//! PC binds (w/a/s/d, f use, r reload, g frag, 4 tactical, v melee, ...). Two spare keys
//! are bound at startup to the console commands +usereload (X) and +speed_throw
//! (hold-to-ADS on LT); see `extra_binds`.
//!
//! In menus (KB_UIState() & 1) the pointer is unlocked and the stick drives an absolute
//! cursor, as with the browser's unlocked mouse; A clicks, B backs out.

use std::collections::HashSet;

/// KB_Input kinds (engine.js).
pub const KEY: i32 = 0;
pub const MOUSE: i32 = 2;
pub const LOCK: i32 = 3;
pub const CURSOR: i32 = 4;
pub const INTRO: i32 = 7;

pub mod key {
    pub const TAB: i32 = 9;
    pub const ENTER: i32 = 13;
    pub const ESC: i32 = 27;
    pub const SPACE: i32 = 32;
    pub const UP: i32 = 154;
    pub const DOWN: i32 = 155;
    pub const LEFT: i32 = 156;
    pub const RIGHT: i32 = 157;
    pub const CTRL: i32 = 159;
    pub const SHIFT: i32 = 160;
    pub const F7: i32 = 173;
    pub const F8: i32 = 174;
    pub const MOUSE1: i32 = 200;
    pub const MOUSE2: i32 = 201;
    pub const WHEEL_DOWN: i32 = 205;
    pub const fn ch(c: char) -> i32 {
        c as i32
    }
}

/// Startup commands that give the pad console-style actions.
pub fn extra_binds() -> Vec<String> {
    ["+bind", "F7", "+usereload", "+bind", "F8", "+speed_throw"].iter().map(|s| s.to_string()).collect()
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

#[derive(Clone, Default, Debug)]
pub struct PadState {
    pub connected: bool,
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

pub struct Mapper {
    held: HashSet<i32>,
    prev: HashSet<Btn>,
    menu: Option<bool>,
    cursor: (f32, f32),
    look_acc: (f32, f32),
    intro_released: bool,
    pub look_speed: f32,
}

fn deadzone(v: f32, dz: f32) -> f32 {
    let a = v.abs();
    if a < dz { 0.0 } else { v.signum() * (a - dz) / (1.0 - dz) }
}

impl Mapper {
    pub fn new() -> Self {
        Self {
            held: HashSet::new(),
            prev: HashSet::new(),
            menu: None,
            cursor: (320.0, 240.0),
            look_acc: (0.0, 0.0),
            intro_released: false,
            look_speed: 1400.0,
        }
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

    fn tap(out: &mut Vec<Event>, k: i32) {
        out.push((KEY, k, 1));
        out.push((KEY, k, 0));
    }

    /// One tick. `dt` seconds, `menu` from KB_UIState, `size` = drawing buffer.
    pub fn update(&mut self, pad: &PadState, dt: f32, menu: bool, size: (i32, i32)) -> Vec<Event> {
        let mut out = Vec::new();
        if !pad.connected {
            if !self.held.is_empty() {
                self.release_all(&mut out);
            }
            self.prev.clear();
            return out;
        }
        let pressed = |b: Btn| pad.buttons.contains(&b) && !self.prev.contains(&b);

        // The engine holds the first loaded frame until the player is "in" (engine.js play()).
        if !self.intro_released && (pressed(Btn::A) || pressed(Btn::Start)) {
            self.intro_released = true;
            out.push((INTRO, 1, 0));
        }

        if self.menu != Some(menu) {
            self.release_all(&mut out);
            out.push((LOCK, (!menu) as i32, 0));
            if menu {
                self.cursor = (size.0 as f32 / 2.0, size.1 as f32 / 2.0);
                out.push((CURSOR, self.cursor.0 as i32, self.cursor.1 as i32));
            }
            self.menu = Some(menu);
        }

        if menu {
            self.menu_tick(pad, dt, size, &mut out);
        } else {
            self.game_tick(pad, dt, &mut out);
        }
        self.prev = pad.buttons.clone();
        out
    }

    fn game_tick(&mut self, pad: &PadState, dt: f32, out: &mut Vec<Event>) {
        let b = |x: Btn| pad.buttons.contains(&x);
        let lx = deadzone(pad.lx, 0.25);
        let ly = deadzone(pad.ly, 0.25);
        self.set(out, key::ch('w'), ly > 0.3);
        self.set(out, key::ch('s'), ly < -0.3);
        self.set(out, key::ch('d'), lx > 0.3);
        self.set(out, key::ch('a'), lx < -0.3);

        self.set(out, key::MOUSE1, pad.rt > 0.35);
        self.set(out, key::F8, pad.lt > 0.35); // +speed_throw (hold ADS)
        self.set(out, key::SPACE, b(Btn::A));
        self.set(out, key::ch('c'), b(Btn::B)); // togglecrouch
        self.set(out, key::F7, b(Btn::X)); // +usereload
        self.set(out, key::ch('1'), b(Btn::Y)); // weapnext
        self.set(out, key::ch('g'), b(Btn::RB)); // lethal
        self.set(out, key::ch('4'), b(Btn::LB)); // tactical
        self.set(out, key::SHIFT, b(Btn::L3)); // sprint
        self.set(out, key::ch('v'), b(Btn::R3)); // melee
        self.set(out, key::ESC, b(Btn::Start));
        self.set(out, key::TAB, b(Btn::Back));
        self.set(out, key::ch('x'), b(Btn::Up)); // actionslot 1
        self.set(out, key::ch('7'), b(Btn::Down)); // actionslot 2
        self.set(out, key::ch('5'), b(Btn::Left)); // actionslot 3
        self.set(out, key::ch('6'), b(Btn::Right)); // actionslot 4

        // Right stick -> relative mouse, squared response like MAP_SQUARED.
        let rx = deadzone(pad.rx, 0.15);
        let ry = deadzone(pad.ry, 0.15);
        let ads = if pad.lt > 0.35 { 0.55 } else { 1.0 };
        self.look_acc.0 += rx * rx.abs() * self.look_speed * ads * dt;
        self.look_acc.1 += -ry * ry.abs() * self.look_speed * 0.75 * ads * dt;
        let dx = self.look_acc.0.trunc();
        let dy = self.look_acc.1.trunc();
        self.look_acc.0 -= dx;
        self.look_acc.1 -= dy;
        if dx != 0.0 || dy != 0.0 {
            out.push((MOUSE, dx as i32, dy as i32));
        }
    }

    fn menu_tick(&mut self, pad: &PadState, dt: f32, size: (i32, i32), out: &mut Vec<Event>) {
        let pressed = |b: Btn| pad.buttons.contains(&b) && !self.prev.contains(&b);
        let released = |b: Btn| !pad.buttons.contains(&b) && self.prev.contains(&b);
        // Either stick moves the cursor; speed scales with the screen.
        let sx = deadzone(if pad.lx.abs() > pad.rx.abs() { pad.lx } else { pad.rx }, 0.2);
        let sy = deadzone(if pad.ly.abs() > pad.ry.abs() { pad.ly } else { pad.ry }, 0.2);
        if sx != 0.0 || sy != 0.0 {
            let speed = size.1.max(480) as f32 * 1.1;
            self.cursor.0 = (self.cursor.0 + sx * sx.abs() * speed * dt).clamp(0.0, (size.0 - 1).max(0) as f32);
            self.cursor.1 = (self.cursor.1 - sy * sy.abs() * speed * dt).clamp(0.0, (size.1 - 1).max(0) as f32);
            out.push((CURSOR, self.cursor.0 as i32, self.cursor.1 as i32));
        }
        if pressed(Btn::A) {
            out.push((CURSOR, self.cursor.0 as i32, self.cursor.1 as i32));
            out.push((KEY, key::MOUSE1, 1));
        }
        if released(Btn::A) {
            out.push((KEY, key::MOUSE1, 0));
        }
        if pressed(Btn::B) || pressed(Btn::Start) {
            Self::tap(out, key::ESC);
        }
        if pressed(Btn::Up) {
            Self::tap(out, key::UP);
        }
        if pressed(Btn::Down) {
            Self::tap(out, key::DOWN);
        }
        if pressed(Btn::Left) {
            Self::tap(out, key::LEFT);
        }
        if pressed(Btn::Right) {
            Self::tap(out, key::RIGHT);
        }
        if pressed(Btn::X) {
            Self::tap(out, key::ENTER);
        }
        if pressed(Btn::Y) {
            Self::tap(out, key::WHEEL_DOWN);
        }
    }
}

// ---------------------------------------------------------------------------------------
// Pad backends

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
        use objc2_game_controller::GCController;
        unsafe {
            let list = GCController::controllers();
            let Some(c) = list.firstObject() else { return PadState::default() };
            let Some(gp) = c.extendedGamepad() else { return PadState::default() };
            let mut s = PadState {
                connected: true,
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
