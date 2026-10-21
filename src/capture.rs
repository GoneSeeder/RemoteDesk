//! Screen capture on the controlled machine, encoded as H.264 video.
//!
//! Each captured frame is compared with the previous one; unchanged screens are not re-encoded,
//! so an idle desktop costs almost nothing. Changed frames go through OpenH264 tuned for screen
//! content, which sends only what changed and is far smaller than per-tile JPEG.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, RateControlMode, UsageType};
use openh264::formats::{RgbaSliceU8, YUVBuffer};

use crate::proto::VideoFrame;

/// Frame-rate cap. 30 fps feels smooth for desktop work without burning CPU.
const FPS: f32 = 30.0;
const MIN_FRAME_TIME: Duration = Duration::from_millis((1000.0 / FPS) as u64);
/// Target bitrate. Screen content compresses well; this keeps text sharp on typical links.
const BITRATE_BPS: u32 = 6_000_000;
/// Send a full keyframe at least this often so a stalled decoder recovers quickly.
const KEYFRAME_EVERY: Duration = Duration::from_secs(5);

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

fn new_encoder() -> Result<Encoder> {
    let config = EncoderConfig::new()
        .bitrate(BitRate::from_bps(BITRATE_BPS))
        .max_frame_rate(FrameRate::from_hz(FPS))
        .usage_type(UsageType::ScreenContentRealTime)
        .rate_control_mode(RateControlMode::Bitrate)
        .skip_frames(false);
    Ok(Encoder::with_api_config(OpenH264API::from_source(), config)?)
}

pub struct Capturer {
    monitor: xcap::Monitor,
    encoder: Option<Encoder>,
    size: (usize, usize),
    prev: Vec<u8>,
    last: Instant,
    last_key: Instant,
}

impl Capturer {
    pub fn new() -> Result<Capturer> {
        Ok(Capturer {
            monitor: primary_monitor()?,
            encoder: None,
            size: (0, 0),
            prev: Vec::new(),
            last: Instant::now() - MIN_FRAME_TIME,
            last_key: Instant::now(),
        })
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

    /// Blocks until the screen has changed and returns it encoded as one H.264 access unit.
    pub fn next_frame(&mut self, stopped: &dyn Fn() -> bool) -> Result<VideoFrame> {
        loop {
            if stopped() {
                anyhow::bail!("session ended");
            }
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
            let (full_w, full_h) = (img.width() as usize, img.height() as usize);
            // YUV 4:2:0 needs even dimensions; drop a trailing column/row if odd.
            let (w, h) = (full_w & !1, full_h & !1);
            if w < 16 || h < 16 {
                continue;
            }
            let mut rgba = img.into_raw();
            if w != full_w || h != full_h {
                rgba = crop(&rgba, full_w, w, h);
            }

            let resized = self.size != (w, h);
            let key_due = self.last_key.elapsed() >= KEYFRAME_EVERY;
            if !resized && !key_due && rgba == self.prev {
                continue; // nothing changed on screen
            }
            if resized || self.encoder.is_none() {
                self.encoder = Some(new_encoder()?);
                self.size = (w, h);
            }
            let encoder = self.encoder.as_mut().unwrap();
            if resized || key_due {
                encoder.force_intra_frame();
                self.last_key = Instant::now();
            }
            let yuv = YUVBuffer::from_rgba8_source(RgbaSliceU8::new(&rgba, (w, h)));
            let data = encoder.encode(&yuv)?.to_vec();
            self.prev = rgba;
            if data.is_empty() {
                continue;
            }
            return Ok(VideoFrame { width: w as u16, height: h as u16, data });
        }
    }
}

fn crop(rgba: &[u8], stride_px: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let start = row * stride_px * 4;
        out.extend_from_slice(&rgba[start..start + w * 4]);
    }
    out
}
