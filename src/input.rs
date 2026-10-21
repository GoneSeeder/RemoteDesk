//! Mouse/keyboard injection on the controlled machine, plus the egui -> Windows virtual-key
//! mapping used by the controlling side.

use std::sync::mpsc::{Receiver, Sender, channel};

use eframe::egui;
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};

use crate::capture::Geometry;
use crate::proto::Input;

const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;

pub enum Cmd {
    Input(Geometry, Input),
    /// Release every held key/button (session ended).
    Reset,
}

/// Starts the single injection thread and returns a sender to it.
pub fn spawn() -> Sender<Cmd> {
    let (tx, rx) = channel();
    crate::localinput::start();
    std::thread::spawn(move || run(rx));
    tx
}

fn run(rx: Receiver<Cmd>) {
    let Ok(mut enigo) = Enigo::new(&Settings::default()) else { return };
    let mut mods = [false; 3]; // ctrl, shift, alt as currently held by us
    let mut held_keys: Vec<u16> = Vec::new();
    let mut held_buttons: Vec<Button> = Vec::new();

    let set_mod = |enigo: &mut Enigo, held: &mut bool, vk: u16, want: bool| {
        if *held != want {
            let dir = if want { Direction::Press } else { Direction::Release };
            let _ = enigo.key(Key::Other(vk as u32), dir);
            *held = want;
        }
    };

    for cmd in rx {
        // The local user wins: drop remote moves and presses while they are active. Releases
        // still go through so nothing stays held down.
        if let Cmd::Input(_, ev) = &cmd {
            let press = matches!(
                ev,
                Input::MouseMove { .. } | Input::Scroll { .. } | Input::Button { down: true, .. } | Input::Key { down: true, .. } | Input::Text(_)
            );
            if press && crate::localinput::local_active() {
                continue;
            }
        }
        match cmd {
            Cmd::Input(g, Input::MouseMove { x, y }) => {
                let px = g.x + (x.clamp(0.0, 1.0) * (g.w.saturating_sub(1)) as f32).round() as i32;
                let py = g.y + (y.clamp(0.0, 1.0) * (g.h.saturating_sub(1)) as f32).round() as i32;
                let _ = enigo.move_mouse(px, py, Coordinate::Abs);
            }
            Cmd::Input(_, Input::Button { button, down }) => {
                let b = match button {
                    1 => Button::Right,
                    2 => Button::Middle,
                    _ => Button::Left,
                };
                let _ = enigo.button(b, if down { Direction::Press } else { Direction::Release });
                held_buttons.retain(|&h| h != b);
                if down {
                    held_buttons.push(b);
                }
            }
            Cmd::Input(_, Input::Scroll { dx, dy }) => {
                if dy != 0 {
                    let _ = enigo.scroll(dy, Axis::Vertical);
                }
                if dx != 0 {
                    let _ = enigo.scroll(dx, Axis::Horizontal);
                }
            }
            Cmd::Input(_, Input::Key { vk, down, ctrl, shift, alt }) => {
                set_mod(&mut enigo, &mut mods[0], VK_CONTROL, ctrl);
                set_mod(&mut enigo, &mut mods[1], VK_SHIFT, shift);
                set_mod(&mut enigo, &mut mods[2], VK_MENU, alt);
                let _ = enigo.key(Key::Other(vk as u32), if down { Direction::Press } else { Direction::Release });
                held_keys.retain(|&k| k != vk);
                if down {
                    held_keys.push(vk);
                }
            }
            Cmd::Input(_, Input::Text(text)) => {
                set_mod(&mut enigo, &mut mods[0], VK_CONTROL, false);
                set_mod(&mut enigo, &mut mods[1], VK_SHIFT, false);
                set_mod(&mut enigo, &mut mods[2], VK_MENU, false);
                let _ = enigo.text(&text);
            }
            Cmd::Input(_, Input::Clipboard(_)) => {}
            Cmd::Reset => {
                for vk in held_keys.drain(..) {
                    let _ = enigo.key(Key::Other(vk as u32), Direction::Release);
                }
                for b in held_buttons.drain(..) {
                    let _ = enigo.button(b, Direction::Release);
                }
                set_mod(&mut enigo, &mut mods[0], VK_CONTROL, false);
                set_mod(&mut enigo, &mut mods[1], VK_SHIFT, false);
                set_mod(&mut enigo, &mut mods[2], VK_MENU, false);
            }
        }
    }
}

/// Maps an egui key to a Windows virtual-key code.
pub fn vk_for(key: egui::Key) -> Option<u16> {
    use egui::Key as K;
    let name = key.name();
    // Letters and digits map directly to their ASCII code.
    if name.len() == 1 {
        let c = name.as_bytes()[0].to_ascii_uppercase();
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u16);
        }
    }
    Some(match key {
        K::Escape => 0x1B,
        K::Tab => 0x09,
        K::Backspace => 0x08,
        K::Enter => 0x0D,
        K::Space => 0x20,
        K::Insert => 0x2D,
        K::Delete => 0x2E,
        K::Home => 0x24,
        K::End => 0x23,
        K::PageUp => 0x21,
        K::PageDown => 0x22,
        K::ArrowLeft => 0x25,
        K::ArrowUp => 0x26,
        K::ArrowRight => 0x27,
        K::ArrowDown => 0x28,
        K::Minus => 0xBD,
        K::Plus | K::Equals => 0xBB,
        K::Comma => 0xBC,
        K::Period => 0xBE,
        K::Semicolon | K::Colon => 0xBA,
        K::Slash | K::Questionmark => 0xBF,
        K::Backtick => 0xC0,
        K::OpenBracket | K::OpenCurlyBracket => 0xDB,
        K::Backslash | K::Pipe => 0xDC,
        K::CloseBracket | K::CloseCurlyBracket => 0xDD,
        K::Quote => 0xDE,
        K::F1 => 0x70,
        K::F2 => 0x71,
        K::F3 => 0x72,
        K::F4 => 0x73,
        K::F5 => 0x74,
        K::F6 => 0x75,
        K::F7 => 0x76,
        K::F8 => 0x77,
        K::F9 => 0x78,
        K::F10 => 0x79,
        K::F11 => 0x7A,
        K::F12 => 0x7B,
        _ => return None,
    })
}
