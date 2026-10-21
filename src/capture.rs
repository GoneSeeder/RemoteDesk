//! Screen capture on the controlled machine. Each frame is compared with the previous one in
//! 64px tiles; only changed tiles are sent, grouped into one JPEG per tile row.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use jpeg_encoder::{ColorType, Encoder};

use crate::proto::{Frame, Rect};

const TILE: usize = 64;
const QUALITY: u8 = 70;
const MIN_FRAME_TIME: Duration = Duration::from_millis(40); // ~25 fps cap

#[derive(Clone, Copy)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

fn primary_monitor() -> Result<xcap::Monitor> {
    let monitors = xcap::Monitor::all().context("list monitors")?;
    let primary = monitors.iter().position(|m| m.is_primary().unwrap_or(false)).unwrap_or(0);
    monitors.into_iter().nth(primary).context("no monitor found")
}

pub struct Capturer {
    monitor: xcap::Monitor,
    prev: Vec<u8>,
    prev_size: (usize, usize),
    last: Instant,
}

impl Capturer {
    pub fn new() -> Result<Capturer> {
        Ok(Capturer { monitor: primary_monitor()?, prev: Vec::new(), prev_size: (0, 0), last: Instant::now() - MIN_FRAME_TIME })
    }

    pub fn geometry(&self) -> Geometry {
        let m = &self.monitor;
        Geometry {
            x: m.x().unwrap_or(0),
            y: m.y().unwrap_or(0),
            w: m.width().unwrap_or(1),
            h: m.height().unwrap_or(1),
        }
    }

    /// Blocks until the screen has changed (or a frame is due) and returns the changed regions.
    pub fn next_frame(&mut self) -> Result<Frame> {
        loop {
            let since = self.last.elapsed();
            if since < MIN_FRAME_TIME {
                std::thread::sleep(MIN_FRAME_TIME - since);
            }
            self.last = Instant::now();

            let img = match self.monitor.capture_image() {
                Ok(img) => img,
                Err(_) => {
                    // Monitor may have been unplugged or resolution changed.
                    std::thread::sleep(Duration::from_millis(300));
                    self.monitor = primary_monitor()?;
                    continue;
                }
            };
            let (w, h) = (img.width() as usize, img.height() as usize);
            let rgba = img.into_raw();

            let full = self.prev_size != (w, h);
            let mut rects = Vec::new();
            for ty in (0..h).step_by(TILE) {
                let th = TILE.min(h - ty);
                let (mut first, mut last) = (usize::MAX, 0);
                for tx in (0..w).step_by(TILE) {
                    let tw = TILE.min(w - tx);
                    if full || tile_changed(&rgba, &self.prev, w, tx, ty, tw, th) {
                        first = first.min(tx);
                        last = tx + tw;
                    }
                }
                if first != usize::MAX {
                    rects.push(encode_rect(&rgba, w, first, ty, last - first, th)?);
                }
            }

            self.prev = rgba;
            self.prev_size = (w, h);
            if !rects.is_empty() {
                return Ok(Frame { screen_w: w as u16, screen_h: h as u16, rects });
            }
        }
    }
}

fn tile_changed(cur: &[u8], prev: &[u8], stride_px: usize, x: usize, y: usize, w: usize, h: usize) -> bool {
    let stride = stride_px * 4;
    (y..y + h).any(|row| {
        let start = row * stride + x * 4;
        cur[start..start + w * 4] != prev[start..start + w * 4]
    })
}

fn encode_rect(rgba: &[u8], stride_px: usize, x: usize, y: usize, w: usize, h: usize) -> Result<Rect> {
    let mut sub = Vec::with_capacity(w * h * 4);
    for row in y..y + h {
        let start = (row * stride_px + x) * 4;
        sub.extend_from_slice(&rgba[start..start + w * 4]);
    }
    let mut jpeg = Vec::new();
    Encoder::new(&mut jpeg, QUALITY).encode(&sub, w as u16, h as u16, ColorType::Rgba)?;
    Ok(Rect { x: x as u16, y: y as u16, w: w as u16, h: h as u16, jpeg })
}
