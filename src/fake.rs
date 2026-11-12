//! SPIKE: a screen for where there is none.
//!
//! With `PEEK_FAKE_SCREEN=2560x1440` set, the engine captures a made-up,
//! unchanging picture of that size instead of the desktop. It is there to
//! measure the rest of the way (encoding, the pipe, the socket) from a
//! process with no display, such as one in Windows session 0. Without the
//! variable this is the real capture and nothing else.

use std::time::{Duration, Instant};

use crate::platform::{Capture, Monitor, Pixels};

pub struct Source {
    real: Option<Capture>,
    fake: Option<Fake>,
    pub last_cost: Duration,
}

struct Fake {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
    /// Handed out once, like a desktop that then stays still.
    fresh: bool,
}

impl Fake {
    /// Mostly flat panels, with a gradient, a block of text-like strokes and
    /// one of grain: about what a desktop compresses to.
    fn new(width: usize, height: usize) -> Self {
        let mut pixels = vec![0u8; width * height * 4];
        let mut seed = 0x2545_f491u32;
        for y in 0..height {
            for x in 0..width {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let panel = ((x / 320) + (y / 240) * 3) % 8;
                let px = &mut pixels[(y * width + x) * 4..][..4];
                match panel {
                    // A smooth gradient.
                    5 => px.copy_from_slice(&[(x * 255 / width) as u8, (y * 255 / height) as u8, 120, 255]),
                    // Text-like: dark strokes on light, a few pixels wide.
                    6 => {
                        let ink = (x / 3 + y / 9 * 7) % 5 == 0 && y % 18 < 11 && (seed >> 30) != 0;
                        let v = if ink { 30 } else { 236 };
                        px.copy_from_slice(&[v, v, v, 255]);
                    }
                    // Photo-like: a gradient with grain.
                    7 => {
                        let grain = (seed >> 30) as u8;
                        px.copy_from_slice(&[(x * 255 / width) as u8 ^ grain, (y * 255 / height) as u8, 120 + grain, 255]);
                    }
                    // Flat UI, most of the screen.
                    _ => {
                        let v = 30 + panel as u8 * 6;
                        px.copy_from_slice(&[v, v, v + 4, 255]);
                    }
                }
            }
        }
        Fake { width, height, pixels, fresh: true }
    }
}

impl Source {
    pub fn new() -> Self {
        let size = std::env::var("PEEK_FAKE_SCREEN").ok().and_then(|v| {
            let (w, h) = v.split_once('x')?;
            Some((w.parse::<usize>().ok()?, h.parse::<usize>().ok()?))
        });
        match size {
            Some((w, h)) if w > 0 && h > 0 && w <= 8192 && h <= 8192 => {
                crate::log!("PEEK_FAKE_SCREEN: showing a made-up {w}x{h} picture, not the desktop");
                Source { real: None, fake: Some(Fake::new(w, h)), last_cost: Duration::ZERO }
            }
            _ => Source { real: Some(Capture::new()), fake: None, last_cost: Duration::ZERO },
        }
    }

    pub fn refresh(&mut self) {
        if let Some(real) = &mut self.real {
            real.refresh();
        }
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        match (&self.real, &self.fake) {
            (Some(real), _) => real.monitors(),
            (_, Some(f)) => vec![Monitor { x: 0, y: 0, width: f.width as u32, height: f.height as u32 }],
            _ => Vec::new(),
        }
    }

    pub fn release(&mut self) {
        if let Some(real) = &mut self.real {
            real.release();
        }
        if let Some(fake) = &mut self.fake {
            // The real capture starts over after a release, and so hands the
            // whole picture out again.
            fake.fresh = true;
        }
    }

    pub fn acquire(&mut self, index: usize, timeout: Duration, f: impl FnOnce(Pixels<'_>)) -> Result<bool, String> {
        if let Some(real) = &mut self.real {
            let got = real.acquire(index, timeout, f).map_err(|e| e.to_string());
            self.last_cost = real.last_cost;
            return got;
        }
        let fake = self.fake.as_mut().expect("a real or a made-up screen");
        if !fake.fresh {
            std::thread::sleep(timeout);
            return Ok(false);
        }
        fake.fresh = false;
        let started = Instant::now();
        f(Pixels { width: fake.width, height: fake.height, stride: fake.width * 4, data: &fake.pixels });
        self.last_cost = started.elapsed();
        Ok(true)
    }
}
