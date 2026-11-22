//! The capture and encode loop.
//!
//! One thread owns the screen buffers and every session's "what does this
//! client still need" state. It only captures while at least one client tab
//! is visible; otherwise it sleeps on its command channel.
//!
//! The screen is cut into 64-pixel tiles. Each captured frame is compared
//! against the previous one tile by tile, changed tiles are merged into
//! rectangles, and each rectangle is sent as a JPEG. With the "lossless"
//! setting, tiles that look like text or flat UI go out as lossless WebP
//! instead, and only photo-like tiles as JPEG.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use jpeg_encoder::{
    ChromaSubsamplingMethod, ColorType, Encoder, ImageBuffer, JpegColorType, SamplingFactor, rgb_to_ycbcr,
};
use rayon::prelude::*;

use crate::platform::{self, Capture, Pixels};
use crate::screen::{Shared, Sink};
use crate::webp;

/// Tile edge in pixels. A multiple of 16 so JPEG blocks line up at the seams.
pub const TILE: usize = 64;
/// Frames sent but not yet drawn by the client. Bounds latency on slow links.
const MAX_INFLIGHT: u32 = 2;
/// Excess send-to-ack delay above the session's baseline RTT that signals TCP
/// queueing (drops to 1 in-flight frame and steps down moving JPEG quality)
/// and the threshold where the pipe is clear again.
const CONGESTED_EXCESS: Duration = Duration::from_millis(100);
const CLEAR_EXCESS: Duration = Duration::from_millis(50);
/// Consecutive congested acks needed before shrinking in-flight / quality.
const CONGESTED_STREAK: u8 = 2;
/// Healthy time required before stepping backed-off moving quality back up.
const BACKOFF_RECOVER: Duration = Duration::from_millis(1500);
/// Minimum moving frame size considered a bandwidth burst for quality backoff.
const LARGE_FRAME_BYTES: usize = 32 * 1024;
/// Stillness needed before blurry regions are re-sent sharply.
const REFINE_AFTER: Duration = Duration::from_millis(100);
/// Hold off sharpening background tiles while full-screen motion (video,
/// full-screen scroll) is actively arriving, so Refine never causes mid-video
/// dips yet still snaps sharp right after pausing.
const LARGE_MOTION_REFINE_HOLD: Duration = Duration::from_millis(120);
/// Short capture wait when there is no way to interrupt the OS call.
const ACQUIRE_WAIT: Duration = Duration::from_millis(10);
#[cfg(target_os = "linux")]
const WAYLAND_WAIT: Duration = Duration::from_secs(1);
#[cfg(windows)]
const INPUT_POLL: Duration = Duration::from_millis(33);
/// With nobody watching for this long, capture resources are released.
const IDLE_RELEASE: Duration = Duration::from_secs(5);
/// A client that has not acknowledged anything for this long is assumed to
/// have lost an ack rather than to still be drawing.
const STALL: Duration = Duration::from_secs(2);
/// Pause between attempts while capture is failing (lock screen, UAC prompt).
const RETRY: Duration = Duration::from_millis(300);
/// Past this many rectangles per frame, fall back to coarser ones.
const MAX_RECTS: usize = 48;
/// Tall rectangles are split into pieces this high and encoded in parallel.
const CHUNK_ROWS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Lowest,
    Low,
    Med,
    High,
}

impl Quality {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "lowest" => Some(Self::Lowest),
            "low" => Some(Self::Low),
            "med" => Some(Self::Med),
            "high" => Some(Self::High),
            _ => None,
        }
    }

    /// Quality index (0 = Lowest .. 3 = High).
    fn level(self) -> u8 {
        match self {
            Self::Lowest => 0,
            Self::Low => 1,
            Self::Med => 2,
            Self::High => 3,
        }
    }

    /// Step down by `steps` levels for moving-frame congestion backoff,
    /// clamping at `Lowest` (still/refine quality is never reduced).
    fn backoff(self, steps: u8) -> Self {
        match self.level().saturating_sub(steps) {
            0 => Self::Lowest,
            1 => Self::Low,
            2 => Self::Med,
            _ => Self::High,
        }
    }

    /// JPEG quality while the region is changing.
    fn moving(self) -> u8 {
        match self {
            Self::Lowest => 30,
            Self::Low => 45,
            Self::Med => 70,
            Self::High => 85,
        }
    }

    /// JPEG quality, and whether to keep full colour resolution, once the
    /// region has settled.
    fn still(self) -> (u8, bool) {
        match self {
            Self::Lowest => (65, false),
            Self::Low => (75, false),
            Self::Med => (90, true),
            Self::High => (95, true),
        }
    }
}

/// Something for a session's socket to send.
pub enum Out {
    Text(String),
    Binary(Vec<u8>),
}

/// How a client wants to be sent the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View {
    pub mon: usize,
    pub quality: Quality,
    /// 1 = full resolution, 2 = half.
    pub scale: usize,
    /// Frames per second at most; 0 = as fast as the host captures.
    pub fps: u32,
    /// Send changing regions at half this resolution; they are re-sent at
    /// full resolution once still.
    pub soft: bool,
    /// Send text and flat UI losslessly (WebP), so it needs no sharp
    /// re-send; photo-like regions still go out as JPEG.
    pub lossless: bool,
}

/// A partial update to a [`View`]; `None` keeps the current value.
#[derive(Default)]
pub struct ViewChange {
    pub mon: Option<usize>,
    pub quality: Option<Quality>,
    pub scale: Option<usize>,
    pub fps: Option<u32>,
    pub soft: Option<bool>,
    pub lossless: Option<bool>,
}

pub enum Cmd {
    Add { id: u64, tx: Sink, view: View, visible: bool },
    Remove { id: u64 },
    Ack { id: u64, seq: u32 },
    Visible { id: u64, visible: bool },
    Configure { id: u64, change: ViewChange },
}

/// The latest image of one monitor, plus which tiles the last capture changed.
#[derive(Default)]
struct Screen {
    width: usize,
    height: usize,
    cols: usize,
    rows: usize,
    buf: Vec<u8>,
    changed: Vec<bool>,
}

impl Screen {
    /// Fold a captured frame into the buffer, recording changed tiles.
    fn update(&mut self, px: Pixels<'_>) {
        let stride = px.width * 4;
        if stride == 0 || px.height == 0 {
            return;
        }
        if (px.width, px.height) != (self.width, self.height) {
            self.width = px.width;
            self.height = px.height;
            self.cols = px.width.div_ceil(TILE);
            self.rows = px.height.div_ceil(TILE);
            self.changed.resize(self.cols * self.rows, true);
            self.changed.fill(true);
            let len = stride * px.height;
            self.buf.clear();
            if !px.y_invert && !px.swap_rb && px.stride == stride {
                self.buf.extend_from_slice(&px.data[..len]);
            } else {
                self.buf.resize(len, 0);
                for (y, dst) in self.buf.chunks_exact_mut(stride).enumerate() {
                    copy_pixels(dst, source_row(&px, y), px.swap_rb);
                }
            }
            return;
        }

        let damage = px.damage.filter(|rects| !rects.is_empty());
        if let Some(rects) = damage {
            self.changed.fill(false);
            for r in rects {
                let right = r.right().min(px.width);
                let bottom = r.bottom().min(px.height);
                if r.x >= right || r.y >= bottom {
                    continue;
                }
                for y in r.y..bottom {
                    let src = source_row(&px, y);
                    let dst = &mut self.buf[y * stride..][..stride];
                    let flags = &mut self.changed[y / TILE * self.cols..][..self.cols];
                    for (col, flag) in flags.iter_mut().enumerate().take(right.div_ceil(TILE)).skip(r.x / TILE) {
                        let start = r.x.max(col * TILE) * 4;
                        let end = right.min((col + 1) * TILE) * 4;
                        diff_pixels(&mut dst[start..end], &src[start..end], flag, px.swap_rb);
                    }
                }
            }
            return;
        }

        let diff_band = |(band, (dst, flags)): (usize, (&mut [u8], &mut [bool]))| {
            flags.fill(false);
            for (row, dst) in dst.chunks_exact_mut(stride).enumerate() {
                let src = source_row(&px, band * TILE + row);
                for ((dst, src), flag) in dst.chunks_mut(TILE * 4).zip(src.chunks(TILE * 4)).zip(&mut *flags) {
                    diff_pixels(dst, src, flag, px.swap_rb);
                }
            }
        };
        if self.rows >= 4 {
            self.buf.par_chunks_mut(stride * TILE)
                .zip(self.changed.par_chunks_mut(self.cols))
                .enumerate()
                .for_each(diff_band);
        } else {
            self.buf.chunks_mut(stride * TILE)
                .zip(self.changed.chunks_mut(self.cols))
                .enumerate()
                .for_each(diff_band);
        }
    }
}

const RGB_MASK: u32 = 0x00FF_FFFF;

fn source_row<'a>(px: &Pixels<'a>, y: usize) -> &'a [u8] {
    let y = if px.y_invert { px.height - 1 - y } else { y };
    &px.data[y * px.stride..][..px.width * 4]
}

fn copy_pixels(dst: &mut [u8], src: &[u8], swap_rb: bool) {
    if swap_rb {
        for (d, s) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src.as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]];
        }
    } else {
        dst.copy_from_slice(src);
    }
}

fn diff_pixels(dst: &mut [u8], src: &[u8], changed: &mut bool, swap_rb: bool) {
    let different = if swap_rb {
        dst.as_chunks::<4>().0.iter().zip(src.as_chunks::<4>().0)
            .any(|(d, s)| [d[0], d[1], d[2]] != [s[2], s[1], s[0]])
    } else {
        // A reduction lets the compiler compare several pixels at once,
        // while the mask removes the undefined byte from every comparison.
        let difference = dst.as_chunks::<4>().0.iter().zip(src.as_chunks::<4>().0)
            .fold(0, |bits, (&d, &s)| bits | (u32::from_le_bytes(d) ^ u32::from_le_bytes(s)));
        difference & RGB_MASK != 0
    };
    if different {
        copy_pixels(dst, src, swap_rb);
        *changed = true;
    }
}

struct Session {
    id: u64,
    tx: Sink,
    view: View,
    /// The requested monitor in `view` clamped to the current layout, as input is.
    mon: usize,
    visible: bool,
    /// Screen size this session's tile state was built for; (0, 0) forces a
    /// full resend.
    dims: (usize, usize),
    tiles: Tiles,
    /// When the oldest change this client has not been sent yet was
    /// captured; reported per frame so the client can show latency.
    pending_since: Option<Instant>,
    seq: u32,
    acked: u32,
    /// Dynamic in-flight frame limit (2 normally, 1 while congested to avoid TCP queue bloat).
    max_inflight: u32,
    /// Lowest send-to-ack round-trip seen in this session (baseline RTT).
    min_rtt: Option<Duration>,
    /// Consecutive acks above `CONGESTED_EXCESS` before backing off.
    slow_acks: u8,
    /// Temporary quality reduction steps applied only to `Pass::Moving` under
    /// sustained queueing delay; recovers gradually once ACKs are clean.
    quality_backoff: u8,
    /// When ACKs last became consistently clean while backed off.
    clear_since: Option<Instant>,
    /// Send metadata of recent frames (`seq`, sent time, large moving burst),
    /// indexed by `seq & 3`, for excess-delay congestion pacing.
    sent_at: [Option<(u32, Instant, bool)>; 4],
    last_send: Instant,
    last_moving: Option<Instant>,
    /// When the last full-screen (>50% tiles) moving frame was sent.
    last_large_motion: Option<Instant>,
    cursor: &'static str,
    blocked: bool,
}

impl Session {
    fn in_flight(&self) -> u32 {
        self.seq.wrapping_sub(self.acked)
    }

    fn send_ready_at(&self) -> Option<Instant> {
        (self.in_flight() >= self.max_inflight).then_some(self.last_send + STALL)
    }

    fn can_capture(&self, now: Instant) -> bool {
        self.visible && (self.dims == (0, 0) || self.send_ready_at().is_none_or(|at| at <= now))
    }

    fn moving_ready_at(&self) -> Option<Instant> {
        self.last_moving.filter(|_| self.view.fps > 0)
            .map(|last| last + Duration::from_secs_f64(1.0 / self.view.fps as f64))
    }

    /// Wait at least `REFINE_AFTER` (100 ms), or two frame intervals when the
    /// client explicitly capped FPS low (e.g. 10 fps -> 200 ms) so motion does
    /// not prematurely refine between slow frames.
    fn refine_after(&self) -> Duration {
        if self.view.fps == 0 {
            return REFINE_AFTER;
        }
        let two_frames = Duration::from_secs_f64(2.0 / self.view.fps as f64);
        REFINE_AFTER.max(two_frames)
    }

    fn large_motion_hold_until(&self) -> Option<Instant> {
        self.last_large_motion.map(|at| at + LARGE_MOTION_REFINE_HOLD.max(self.refine_after()))
    }

    fn record_sent(&mut self, seq: u32, at: Instant, large_moving: bool) {
        self.sent_at[(seq as usize) & 3] = Some((seq, at, large_moving));
    }

    fn note_ack(&mut self, seq: u32, now: Instant) {
        if let Some((s, at, large_moving)) = self.sent_at[(seq as usize) & 3]
            && s == seq
        {
            let rtt = now.duration_since(at);
            let min_rtt = match self.min_rtt {
                Some(min) if min <= rtt => min,
                _ => {
                    self.min_rtt = Some(rtt);
                    rtt
                }
            };
            let excess = rtt.saturating_sub(min_rtt);
            if excess >= CONGESTED_EXCESS {
                self.clear_since = None;
                self.slow_acks = self.slow_acks.saturating_add(1);
                if self.slow_acks >= CONGESTED_STREAK {
                    self.max_inflight = 1;
                    if large_moving && self.quality_backoff < self.view.quality.level() {
                        self.quality_backoff += 1;
                        self.slow_acks = 0;
                    }
                }
            } else {
                self.slow_acks = 0;
                if excess <= CLEAR_EXCESS {
                    self.max_inflight = MAX_INFLIGHT;
                    if self.quality_backoff > 0 {
                        let since = *self.clear_since.get_or_insert(now);
                        if now.duration_since(since) >= BACKOFF_RECOVER {
                            self.quality_backoff -= 1;
                            self.clear_since = (self.quality_backoff > 0).then_some(now);
                        }
                    }
                } else {
                    self.clear_since = None;
                }
            }
        }
    }

    fn send_text(&self, text: String) {
        (self.tx)(Out::Text(text));
    }
}

/// What one client has of each tile, and so what it should be sent next.
#[derive(Default)]
struct Tiles {
    /// Tiles the client does not have the current content of.
    dirty: Vec<bool>,
    /// Tiles the client has, but only at the lower "moving" quality.
    lossy: Vec<bool>,
    /// When each tile last went out as a moving update.
    changed_at: Vec<Instant>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pass {
    /// The whole picture, sharp: just connected or the view changed.
    Fresh,
    /// Regions that changed, at the "moving" quality.
    Moving,
    /// Regions that went out blurry and have since been still, sharp.
    Refine,
}

impl Tiles {
    fn reset(&mut self, count: usize, now: Instant) {
        self.dirty = vec![true; count];
        self.lossy = vec![false; count];
        self.changed_at = vec![now; count];
    }

    fn refine_deadlines(&self, refine_after: Duration) -> impl Iterator<Item = (usize, Instant)> + '_ {
        self.lossy.iter().zip(&self.dirty).zip(&self.changed_at).enumerate()
            .filter(|&(_, ((lossy, dirty), _))| *lossy && !*dirty)
            .map(move |(t, (_, changed_at))| (t, *changed_at + refine_after))
    }

    /// Pick the next frame's tiles. Live motion always takes priority over
    /// sharpening, and sharpening only runs when `refine_ready` (pipe idle)
    /// so heavy motion never triggers a large-refine congestion spiral.
    fn next(
        &mut self,
        now: Instant,
        fresh: bool,
        moving_ready: bool,
        refine_ready: bool,
        refine_after: Duration,
    ) -> Option<(Vec<bool>, Pass)> {
        if fresh {
            self.lossy.fill(false);
            let blank = vec![false; self.dirty.len()];
            return Some((std::mem::replace(&mut self.dirty, blank), Pass::Fresh));
        }
        if moving_ready && self.dirty.contains(&true) {
            for t in 0..self.dirty.len() {
                if self.dirty[t] {
                    self.lossy[t] = true;
                    self.changed_at[t] = now;
                }
            }
            let blank = vec![false; self.dirty.len()];
            return Some((std::mem::replace(&mut self.dirty, blank), Pass::Moving));
        }
        if refine_ready {
            let settled = {
                let mut ready = self.refine_deadlines(refine_after).filter(|&(_, at)| at <= now);
                ready.next().map(|(first, _)| {
                    let mut settled = vec![false; self.dirty.len()];
                    settled[first] = true;
                    for (t, _) in ready {
                        settled[t] = true;
                    }
                    settled
                })
            };
            if let Some(settled) = settled {
                for (lossy, &refined) in self.lossy.iter_mut().zip(&settled) {
                    *lossy &= !refined;
                }
                return Some((settled, Pass::Refine));
            }
        }
        None
    }

    /// Record what a frame drew (see [`Frame`]): drawn tiles now have that
    /// frame's quality, blurry for a moving pass unless drawn losslessly.
    fn sent(&mut self, drawn: &[bool], exact: &[bool], moving: bool, now: Instant) {
        for t in (0..drawn.len()).filter(|&t| drawn[t]) {
            let lossy = moving && !exact.get(t).copied().unwrap_or(false);
            if lossy && !self.lossy[t] {
                self.changed_at[t] = now;
            }
            self.lossy[t] = lossy;
        }
    }
}

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

/// A rectangle in tile coordinates (half-open `[c0, c1) x [r0, r1)`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TileRect {
    c0: usize,
    r0: usize,
    c1: usize,
    r1: usize,
}

impl TileRect {
    fn area(self) -> usize {
        (self.c1 - self.c0) * (self.r1 - self.r0)
    }

    fn union(self, other: Self) -> Self {
        Self {
            c0: self.c0.min(other.c0),
            r0: self.r0.min(other.r0),
            c1: self.c1.max(other.c1),
            r1: self.r1.max(other.r1),
        }
    }

    fn extra_tiles(self, other: Self) -> usize {
        let u = self.union(other);
        let iw = self.c1.min(other.c1).saturating_sub(self.c0.max(other.c0));
        let ih = self.r1.min(other.r1).saturating_sub(self.r0.max(other.r0));
        u.area() - (self.area() + other.area() - iw * ih)
    }

    fn to_pixels(self, width: usize, height: usize) -> Rect {
        let (x, y) = (self.c0 * TILE, self.r0 * TILE);
        Rect {
            x,
            y,
            w: (self.c1 * TILE).min(width) - x,
            h: (self.r1 * TILE).min(height) - y,
        }
    }
}

/// Merge flagged tiles into rectangles: horizontal runs per tile row, then
/// runs with identical extents on consecutive rows are stacked.
#[cfg(test)]
fn merge(tiles: &[bool], cols: usize, rows: usize, width: usize, height: usize) -> Vec<Rect> {
    merge_with_coalesce(tiles, cols, rows, width, height, false)
}

/// Like [`merge`], optionally coalescing nearby fragmented rectangles so
/// video/scrolling does not send dozens of tiny JPEGs with repeated headers.
fn merge_with_coalesce(tiles: &[bool], cols: usize, rows: usize, width: usize, height: usize, do_coalesce: bool) -> Vec<Rect> {
    let mut rects = stack_tiles(tiles, cols, rows, false);
    if rects.len() > MAX_RECTS {
        rects = stack_tiles(tiles, cols, rows, true);
    }
    if do_coalesce && rects.len() > 1 {
        coalesce(&mut rects);
    }
    rects.into_iter().map(|r| r.to_pixels(width, height)).collect()
}

/// Merge nearby tile rectangles when dropping an extra JPEG header (~630 B),
/// seam, and browser decode call costs less than encoding a few unchanged tiles.
fn coalesce(rects: &mut Vec<TileRect>) {
    loop {
        let mut best: Option<(usize, usize, usize)> = None;
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let u = rects[i].union(rects[j]);
                let extra = rects[i].extra_tiles(rects[j]);
                if extra <= 4 || (extra <= 24 && extra * 5 <= u.area()) {
                    if best.is_none_or(|(b_extra, _, _)| extra < b_extra) {
                        best = Some((extra, i, j));
                        if extra == 0 {
                            break;
                        }
                    }
                }
            }
            if best.is_some_and(|(e, _, _)| e == 0) {
                break;
            }
        }
        let Some((_, i, j)) = best else { break };
        rects[i] = rects[i].union(rects[j]);
        rects.swap_remove(j);
    }
}

/// Rectangles of flagged tiles: runs per tile row (`coarse`: one run from the
/// first flagged tile to the last), stacked while consecutive rows match.
fn stack(tiles: &[bool], cols: usize, rows: usize, width: usize, height: usize, coarse: bool) -> Vec<Rect> {
    stack_tiles(tiles, cols, rows, coarse)
        .into_iter()
        .map(|r| r.to_pixels(width, height))
        .collect()
}

fn stack_tiles(tiles: &[bool], cols: usize, rows: usize, coarse: bool) -> Vec<TileRect> {
    let mut done: Vec<TileRect> = Vec::new();
    let mut open: Vec<(usize, usize, usize)> = Vec::new(); // (col0, col1, row0)
    for r in 0..=rows {
        let mut runs: Vec<(usize, usize)> = Vec::new();
        if r < rows {
            let row = &tiles[r * cols..(r + 1) * cols];
            if coarse {
                let first = row.iter().position(|&t| t);
                let last = row.iter().rposition(|&t| t);
                if let (Some(a), Some(b)) = (first, last) {
                    runs.push((a, b + 1));
                }
            } else {
                let mut c = 0;
                while c < cols {
                    if row[c] {
                        let start = c;
                        while c < cols && row[c] {
                            c += 1;
                        }
                        runs.push((start, c));
                    } else {
                        c += 1;
                    }
                }
            }
        }
        let mut next = Vec::with_capacity(runs.len());
        for &(c0, c1, r0) in &open {
            if runs.contains(&(c0, c1)) {
                next.push((c0, c1, r0));
            } else {
                done.push(TileRect { c0, r0, c1, r1: r });
            }
        }
        for &(c0, c1) in &runs {
            if !open.iter().any(|&(a, b, _)| (a, b) == (c0, c1)) {
                next.push((c0, c1, r));
            }
        }
        open = next;
    }
    done
}

/// Visit one row of rounded block averages. Full blocks use a constant
/// divisor of 4 or 16, and only partial edge blocks need a dynamic divisor.
fn downscale_row<const F: usize>(screen: &Screen, r: Rect, oy: usize, mut emit: impl FnMut([u8; 3])) {
    let stride = screen.width * 4;
    let y = r.y + oy * F;
    let rows = F.min(r.y + r.h - y);
    let first = y * stride + r.x * 4;
    let lines: [&[[u8; 4]]; F] = std::array::from_fn(|dy| {
        let offset = first + dy.min(rows - 1) * stride;
        screen.buf[offset..][..r.w * 4].as_chunks::<4>().0
    });
    let full_width = r.w / F * F;
    if rows == F {
        for x in (0..full_width).step_by(F) {
            let mut sum = [0u16; 3];
            for line in lines {
                for &[b, g, r, _] in &line[x..][..F] {
                    sum[0] += u16::from(b);
                    sum[1] += u16::from(g);
                    sum[2] += u16::from(r);
                }
            }
            let shift = (F * F).trailing_zeros();
            emit(sum.map(|v| ((v + (F * F / 2) as u16) >> shift) as u8));
        }
    }
    let edge_start = if rows == F { full_width } else { 0 };
    for x in (edge_start..r.w).step_by(F) {
        let cols = F.min(r.w - x);
        let mut sum = [0u16; 3];
        for line in &lines[..rows] {
            for &[b, g, r, _] in &line[x..x + cols] {
                sum[0] += u16::from(b);
                sum[1] += u16::from(g);
                sum[2] += u16::from(r);
            }
        }
        let count = (rows * cols) as u16;
        emit(sum.map(|v| ((v + count / 2) / count) as u8));
    }
}

/// Average blocks into packed BGRA so the JPEG encoder can use its
/// SIMD color conversion at reduced resolutions.
fn downscale(screen: &Screen, r: Rect, f: usize) -> (Vec<u8>, usize, usize) {
    let (ow, oh) = (r.w.div_ceil(f), r.h.div_ceil(f));
    let mut out = Vec::with_capacity(ow * oh * 4);
    for oy in 0..oh {
        let emit = |[b, g, r]: [u8; 3]| out.extend_from_slice(&[b, g, r, 255]);
        match f {
            2 => downscale_row::<2>(screen, r, oy, emit),
            4 => downscale_row::<4>(screen, r, oy, emit),
            _ => unreachable!("unsupported JPEG scale {f}"),
        }
    }
    (out, ow, oh)
}

/// A JPEG region borrowed from the screen, with no packed intermediate.
struct StridedBgra<'a> {
    data: &'a [u8],
    stride: usize,
    width: u16,
    height: u16,
}

impl ImageBuffer for StridedBgra<'_> {
    fn get_jpeg_color_type(&self) -> JpegColorType {
        JpegColorType::Ycbcr
    }

    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn fill_buffers(&self, y: u16, buffers: &mut [Vec<u8>; 4]) {
        let line = &self.data[usize::from(y) * self.stride..][..usize::from(self.width) * 4];
        let [luma, cb, cr, _] = buffers;
        for &[b, g, r, _] in line.as_chunks::<4>().0 {
            let (y, u, v) = rgb_to_ycbcr(r, g, b);
            luma.push(y);
            cb.push(u);
            cr.push(v);
        }
    }
}

/// Where a region shrunk by `scale` goes in the client's canvas.
/// Rectangles start on tile (or chunk) boundaries, so x and y divide evenly.
fn on_canvas(r: Rect, scale: usize) -> Rect {
    Rect { x: r.x / scale, y: r.y / scale, w: r.w.div_ceil(scale), h: r.h.div_ceil(scale) }
}

/// JPEG-encode one region, shrunk by `factor`. Returns where it goes in the
/// client's canvas (which is the screen shrunk by `scale`) and the bytes; the
/// client stretches the image to fill that rectangle.
fn encode_piece(screen: &Screen, r: Rect, scale: usize, factor: usize, quality: u8, full_chroma: bool) -> (Rect, Vec<u8>) {
    let (ow, oh) = (r.w.div_ceil(factor), r.h.div_ceil(factor));
    let mut out = Vec::with_capacity(ow * oh / 3 + 1024);
    let mut encoder = Encoder::new(&mut out, quality);
    encoder.set_sampling_factor(if full_chroma {
        SamplingFactor::R_4_4_4
    } else {
        SamplingFactor::R_4_2_0
    });
    encoder.set_chroma_subsampling_method(ChromaSubsamplingMethod::Average);
    let stride = screen.width * 4;
    let data = &screen.buf[r.y * stride + r.x * 4..];
    let result = if factor != 1 {
        let (pixels, _, _) = downscale(screen, r, factor);
        encoder.encode(&pixels, ow as u16, oh as u16, ColorType::Bgra)
    } else if r.w == screen.width {
        // Full-width rows are already packed, so keep SIMD colour conversion.
        encoder.encode(data, ow as u16, oh as u16, ColorType::Bgra)
    } else {
        encoder.encode_image(StridedBgra { data, stride, width: ow as u16, height: oh as u16 })
    };
    result.expect("JPEG encoding into memory cannot fail");
    (on_canvas(r, scale), out)
}

/// Colour of tiles painted over in a lossless piece (they go out as JPEG).
const FLAT: [u8; 3] = [128, 128, 128];

/// Losslessly encode one region at the canvas scale as WebP. Read each exact
/// tile span in place, or average it at half size. JPEG covers the flat spans.
fn encode_lossless(screen: &Screen, r: Rect, scale: usize, exact: &[bool]) -> (Rect, Vec<u8>) {
    let (ow, oh) = (r.w.div_ceil(scale), r.h.div_ceil(scale));
    let out = webp::encode(ow, oh, |oy, row| {
        let y = r.y + oy * scale;
        let tiles = &exact[y / TILE * screen.cols + r.x / TILE..];
        for (col, (span, &is_exact)) in row.chunks_mut(TILE / scale).zip(tiles).enumerate() {
            if !is_exact {
                span.fill(FLAT);
                continue;
            }
            let x = r.x + col * TILE;
            if scale == 1 {
                let line = &screen.buf[(y * screen.width + x) * 4..][..span.len() * 4];
                for (dst, &[b, g, r, _]) in span.iter_mut().zip(line.as_chunks::<4>().0) {
                    *dst = [r, g, b];
                }
            } else {
                let tile = Rect { x, w: TILE.min(r.x + r.w - x), ..r };
                let mut dst = span.iter_mut();
                downscale_row::<2>(screen, tile, oy, |[b, g, r]| *dst.next().unwrap() = [r, g, b]);
            }
        }
    });
    (on_canvas(r, scale), out)
}

/// Whether at least three quarters of a tile's pixels repeat the one to
/// their left or above. Ignore the undefined fourth byte.
fn looks_flat(screen: &Screen, tile: usize) -> bool {
    let (x0, y0) = (tile % screen.cols * TILE, tile / screen.cols * TILE);
    let (w, h) = (TILE.min(screen.width - x0), TILE.min(screen.height - y0));
    let stride = screen.width * 4;
    let read = |&pixel: &[u8; 4]| u32::from_le_bytes(pixel) & RGB_MASK;
    let max_misses = w * h / 4;
    let min_repeats = w * h - max_misses;
    let mut misses = 0;
    let mut repeats = 0;
    let mut above = None;
    for y in y0..y0 + h {
        let row = screen.buf[y * stride + x0 * 4..][..w * 4].as_chunks::<4>().0;
        let mut prev_x = u32::MAX;
        for (x, value) in row.iter().map(read).enumerate() {
            if value == prev_x || above.is_some_and(|row: &[[u8; 4]]| value == read(&row[x])) {
                repeats += 1;
                if repeats >= min_repeats {
                    return true;
                }
            } else {
                misses += 1;
                if misses > max_misses {
                    return false;
                }
            }
            prev_x = value;
        }
        above = Some(row);
    }
    repeats >= min_repeats
}

/// Bridge small flat gaps between photo tiles within a covered row run.
fn photo_spans(covered: &[bool], photo: &[bool], cols: usize) -> Vec<bool> {
    const MAX_PHOTO_GAP: usize = 2;
    let mut spans = vec![false; covered.len()];
    for row in (0..covered.len()).step_by(cols) {
        let mut last_photo = None;
        for t in row..row + cols {
            if !covered[t] {
                last_photo = None;
            } else if photo[t] {
                let from = last_photo.filter(|&last| t - last <= MAX_PHOTO_GAP + 1).unwrap_or(t);
                spans[from..=t].fill(true);
                last_photo = Some(t);
            }
        }
    }
    spans
}

/// Tight tile-aligned bounds of the exact pixels within a lossless chunk.
fn exact_bounds(r: Rect, exact: &[bool], cols: usize) -> Option<Rect> {
    let mut left = r.x + r.w;
    let mut top = r.y + r.h;
    let mut right = r.x;
    let mut bottom = r.y;
    for y in (r.y..r.y + r.h).step_by(TILE) {
        for x in (r.x..r.x + r.w).step_by(TILE) {
            if exact[y / TILE * cols + x / TILE] {
                left = left.min(x);
                top = top.min(y);
                right = right.max((x + TILE).min(r.x + r.w));
                bottom = bottom.max((y + TILE).min(r.y + r.h));
            }
        }
    }
    (left < right).then(|| Rect { x: left, y: top, w: right - left, h: bottom - top })
}

/// Mark the tiles a list of rectangles covers.
fn cover(rects: &[Rect], cols: usize, tiles: &mut [bool]) {
    for r in rects {
        for ty in r.y / TILE..(r.y + r.h).div_ceil(TILE) {
            tiles[ty * cols + r.x / TILE..ty * cols + (r.x + r.w).div_ceil(TILE)].fill(true);
        }
    }
}

/// Cut rectangles into pieces at most `max_rows` high, to encode in parallel.
fn chunks(rects: Vec<Rect>, max_rows: usize) -> Vec<Rect> {
    let mut pieces = Vec::new();
    for r in rects {
        let mut y = r.y;
        while y < r.y + r.h {
            let h = max_rows.min(r.y + r.h - y);
            pieces.push(Rect { y, h, ..r });
            y += h;
        }
    }
    pieces
}

/// Bytes before the first rectangle.
const HEADER: usize = 17;
/// Frame flag: colour kept at full resolution (JPEG 4:4:4 instead of 4:2:0).
const FLAG_FULL_CHROMA: u8 = 1;
/// Frame flag: a sharp re-send of regions that went out blurry.
const FLAG_REFINE: u8 = 2;
/// Frame flag: encoded at half the canvas resolution, to be stretched.
const FLAG_SOFT: u8 = 4;
/// Frame flag: each rectangle carries a format byte (0 JPEG, 1 lossless
/// WebP) between its position and its length.
const FLAG_LOSSLESS: u8 = 8;

/// One frame message, and what it does to the client's picture.
struct Frame {
    msg: Vec<u8>,
    /// Tiles the frame draws: those asked for, and more when a coarse merge
    /// takes in whole rows.
    drawn: Vec<bool>,
    /// Of those, the ones drawn losslessly (none without FLAG_LOSSLESS).
    exact: Vec<bool>,
}

/// Build one frame message for a canvas of the screen shrunk by `scale`.
///
/// Layout (little-endian): u8 kind = 1, u32 seq, u16 width, u16 height,
/// u16 count, u32 host time in microseconds (filled in by the caller once
/// encoding is done), u8 JPEG quality, u8 flags; then per rectangle u16 x,
/// u16 y, u16 w, u16 h (where to draw it on the canvas), with FLAG_LOSSLESS
/// a u8 format (0 JPEG, 1 WebP), then u32 length and the image bytes. A JPEG
/// may be smaller than w x h; it is stretched to fit. Rectangles are drawn
/// in order: lossless pieces come first, with their photo-like tiles painted
/// flat, and JPEG rectangles over those tiles after.
fn frame_message(screen: &Screen, tiles: &[bool], scale: usize, quality: u8, flags: u8, seq: u32) -> Frame {
    let full_chroma = flags & FLAG_FULL_CHROMA != 0;
    let factor = if flags & FLAG_SOFT != 0 { scale * 2 } else { scale };
    let jpeg_only = flags & FLAG_LOSSLESS == 0;
    let rects = merge_with_coalesce(tiles, screen.cols, screen.rows, screen.width, screen.height, jpeg_only);
    let mut covered = vec![false; tiles.len()];
    cover(&rects, screen.cols, &mut covered);
    let jpeg_chunk_rows = CHUNK_ROWS * factor;
    let (lossless, jpeg, exact) = if !jpeg_only {
        let mut exact: Vec<bool> = (0..tiles.len()).into_par_iter().map(|t| covered[t] && looks_flat(screen, t)).collect();
        let photo: Vec<bool> = covered.iter().zip(&exact).map(|(&c, &e)| c && !e).collect();
        // Many small JPEGs would cost more in headers and seams than drawing
        // over some flat tiles, which are then not exact after all.
        let spans = photo_spans(&covered, &photo, screen.cols);
        let jpeg = stack(&spans, screen.cols, screen.rows, screen.width, screen.height, false);
        for (e, &d) in exact.iter_mut().zip(&spans) {
            *e &= !d;
        }
        let lossless: Vec<Rect> = chunks(rects, CHUNK_ROWS).into_iter().filter_map(|r| exact_bounds(r, &exact, screen.cols)).collect();
        (lossless, chunks(jpeg, jpeg_chunk_rows), exact)
    } else {
        (Vec::new(), chunks(rects, jpeg_chunk_rows), Vec::new())
    };
    let jobs: Vec<(Rect, bool)> = lossless.iter().map(|&r| (r, true)).chain(jpeg.iter().map(|&r| (r, false))).collect();
    let parts: Vec<(Rect, bool, Vec<u8>)> = jobs
        .par_iter()
        .map(|&(r, webp)| {
            let (at, data) = if webp {
                encode_lossless(screen, r, scale, &exact)
            } else {
                encode_piece(screen, r, scale, factor, quality, full_chroma)
            };
            (at, webp, data)
        })
        .collect();

    let mut msg = Vec::with_capacity(HEADER + parts.iter().map(|(_, _, d)| d.len() + 13).sum::<usize>());
    msg.push(1u8);
    msg.extend_from_slice(&seq.to_le_bytes());
    msg.extend_from_slice(&(screen.width.div_ceil(scale) as u16).to_le_bytes());
    msg.extend_from_slice(&(screen.height.div_ceil(scale) as u16).to_le_bytes());
    msg.extend_from_slice(&(parts.len() as u16).to_le_bytes());
    msg.extend_from_slice(&0u32.to_le_bytes());
    msg.push(quality);
    msg.push(flags);
    for (r, webp, data) in &parts {
        for v in [r.x, r.y, r.w, r.h] {
            msg.extend_from_slice(&(v as u16).to_le_bytes());
        }
        if flags & FLAG_LOSSLESS != 0 {
            msg.push(*webp as u8);
        }
        msg.extend_from_slice(&(data.len() as u32).to_le_bytes());
        msg.extend_from_slice(data);
    }
    Frame { msg, drawn: covered, exact }
}

#[derive(Default)]
struct Stats {
    frames: u32,
    bytes: usize,
    capture: Duration,
    encode: Duration,
    since: Option<Instant>,
}

struct Engine {
    cap: Capture,
    screens: HashMap<usize, Screen>,
    sessions: Vec<Session>,
    shared: Arc<Shared>,
    interval: Duration,
    last_capture: Option<Instant>,
    wanted: Vec<usize>,
    poll_capture: bool,
    #[cfg(windows)]
    input_poll_at: Option<Instant>,
    retry_at: Option<Instant>,
    failing: bool,
    stats: Stats,
}

/// The fastest any watching client wants (0 = no cap), but never faster
/// than the host's own limit.
fn capture_interval(host: Duration, caps: impl Iterator<Item = u32>) -> Duration {
    caps.map(|fps| if fps == 0 { host } else { Duration::from_secs_f64(1.0 / fps as f64) })
        .min()
        .unwrap_or(host)
        .max(host)
}

pub fn run(rx: Receiver<Cmd>, shared: Arc<Shared>, fps: u32) {
    let mut e = Engine {
        cap: Capture::new(),
        screens: HashMap::new(),
        sessions: Vec::new(),
        shared,
        interval: Duration::from_secs_f64(1.0 / fps as f64),
        last_capture: None,
        wanted: Vec::new(),
        poll_capture: false,
        #[cfg(windows)]
        input_poll_at: None,
        retry_at: None,
        failing: false,
        stats: Stats::default(),
    };
    #[cfg(target_os = "linux")]
    e.cap.set_wakeup(e.shared.engine.wakeup.clone());
    e.publish_monitors();
    loop {
        #[cfg(target_os = "linux")]
        if let Some(wakeup) = &e.shared.engine.wakeup {
            wakeup.clear();
        }
        while let Ok(cmd) = rx.try_recv() {
            e.handle(cmd);
        }
        if !e.sessions.iter().any(|s| s.visible) {
            let cmd = match rx.recv_timeout(IDLE_RELEASE) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    e.cap.release();
                    e.screens.clear();
                    e.last_capture = None;
                    match rx.recv() {
                        Ok(cmd) => cmd,
                        Err(_) => return,
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };
            e.handle(cmd);
            continue;
        }

        let mut now = Instant::now();
        let room = e.sessions.iter().any(|s| s.can_capture(now));
        let mut send_at = e.send_deadline(now);
        if send_at.is_some_and(|at| at <= now) {
            // Always poll live capture before flushing a due refine or
            // backlog so we never mistake an un-polled video for a still screen.
            let captured = if room && e.retry_at.is_none_or(|at| at <= now) {
                e.capture(Duration::ZERO, now)
            } else {
                false
            };
            e.flush_all(captured);
            now = Instant::now();
            send_at = e.send_deadline(now);
        }
        let room = e.sessions.iter().any(|s| s.can_capture(now));
        let first = e.sessions.iter().any(|s| s.visible && s.dims == (0, 0) && !e.has_pixels(s.mon));
        let poll_capture = e.poll_capture;
        let ready_at = e.retry_at.or_else(|| {
            e.last_capture.filter(|_| !first && !poll_capture).map(|last| last + e.capture_interval())
        });
        if !room || ready_at.is_some_and(|at| at > now) {
            // Acknowledgements can wake this wait immediately when all
            // clients are backed up, including on Windows.
            let mut wait = if room { ready_at.unwrap() - now } else { ACQUIRE_WAIT };
            if let Some(at) = send_at {
                wait = wait.min(at.saturating_duration_since(now));
            }
            #[cfg(windows)]
            {
                e.poll_input(now, false);
                wait = wait.min(e.input_poll_at.unwrap().saturating_duration_since(now));
            }
            match rx.recv_timeout(wait) {
                Ok(cmd) => e.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }

        e.retry_at = None;
        let poll_capture = std::mem::take(&mut e.poll_capture);
        let mut wait = if first || poll_capture { Duration::ZERO } else { e.capture_wait() };
        if let Some(at) = send_at {
            wait = wait.min(at.saturating_duration_since(now));
        }
        let captured = e.capture(wait, now);
        if captured || poll_capture {
            e.flush_all(captured);
        }
        #[cfg(windows)]
        if !captured {
            e.poll_input(Instant::now(), false);
        }
    }
}

impl Engine {
    /// Time between captures: the host's limit, or slower if every client
    /// watching asked for fewer frames. Changes accumulate in between, so a
    /// slower rate sends less without losing anything.
    fn capture_interval(&self) -> Duration {
        capture_interval(self.interval, self.sessions.iter().filter(|s| s.visible).map(|s| s.view.fps))
    }

    fn has_pixels(&self, mon: usize) -> bool {
        self.screens.get(&mon).is_some_and(|screen| screen.width > 0)
    }

    fn capture_wait(&self) -> Duration {
        #[cfg(target_os = "linux")]
        if self.shared.engine.wakeup.is_some() {
            // A single monitor's pending request can wait for the Wayland
            // fd. With several, periodically check each one's queue state.
            let mut visible = self.sessions.iter().filter(|s| s.visible);
            if let Some(first) = visible.next() {
                if visible.all(|s| s.mon == first.mon) {
                    return WAYLAND_WAIT;
                }
            }
        }
        ACQUIRE_WAIT
    }

    /// The next pending send, including a capture-independent refine.
    fn send_deadline(&self, now: Instant) -> Option<Instant> {
        self.sessions.iter().filter(|s| s.visible).filter_map(|s| {
            let screen = self.screens.get(&s.mon).filter(|screen| screen.width > 0)?;
            let pending = if s.dims != (screen.width, screen.height) {
                now
            } else {
                let moving = s.tiles.dirty.contains(&true).then(|| s.moving_ready_at().unwrap_or(now));
                let refine = (s.in_flight() == 0)
                    .then(|| {
                        let due = s.tiles.refine_deadlines(s.refine_after()).map(|(_, at)| at).min()?;
                        Some(s.large_motion_hold_until().map_or(due, |hold| due.max(hold)))
                    })
                    .flatten();
                moving.into_iter().chain(refine).min()?
            };
            Some(pending.max(s.send_ready_at().unwrap_or(now)))
        }).min()
    }

    /// Returns true when the layout changed and there is a display to capture.
    fn publish_monitors(&mut self) -> bool {
        let monitors = self.cap.monitors();
        let mut shared = self.shared.monitors.write().unwrap();
        if *shared == monitors {
            return false;
        }
        crate::log!(
            "monitors: {}",
            monitors
                .iter()
                .map(|m| format!("{}x{}@{},{}", m.width, m.height, m.x, m.y))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let available = !monitors.is_empty();
        *shared = monitors;
        drop(shared);
        // Cached frames belong to the old layout. An index may even refer to
        // a different screen now, so wait for a new capture before flushing.
        self.screens.clear();
        for i in 0..self.sessions.len() {
            self.hello(i);
        }
        available
    }

    fn hello(&mut self, index: usize) {
        let monitors = self.shared.monitors.read().unwrap().clone();
        let s = &mut self.sessions[index];
        s.mon = s.view.mon.min(monitors.len().saturating_sub(1));
        s.dims = (0, 0);
        s.pending_since = None;
        let list: Vec<_> = monitors
            .iter()
            .map(|m| serde_json::json!({ "w": m.width, "h": m.height }))
            .collect();
        s.send_text(
            serde_json::json!({
                "t": "hello",
                "monitors": list,
                "mon": s.mon,
                "viewOnly": self.shared.view_only,
                "clipboard": self.shared.clipboard,
            })
            .to_string(),
        );
    }

    fn flush_fresh(&mut self, index: usize, was_watching: bool, now: Instant) {
        if !self.sessions[index].visible {
            return;
        }
        let mon = self.sessions[index].mon;
        if !self.has_pixels(mon) {
            return;
        }
        // If no visible session was watching this monitor, poll once without
        // blocking so a warm cached buffer picks up any change since it paused.
        if !was_watching {
            self.capture(Duration::ZERO, now);
        }
        self.flush(index, Instant::now());
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Add { id, tx, view, visible } => {
                let now = Instant::now();
                self.sessions.push(Session {
                    id,
                    tx,
                    view,
                    mon: 0,
                    visible,
                    dims: (0, 0),
                    tiles: Tiles::default(),
                    pending_since: None,
                    seq: 0,
                    acked: 0,
                    max_inflight: MAX_INFLIGHT,
                    min_rtt: None,
                    slow_acks: 0,
                    quality_backoff: 0,
                    clear_since: None,
                    sent_at: [None; 4],
                    last_send: now,
                    last_moving: None,
                    last_large_motion: None,
                    cursor: "",
                    blocked: false,
                });
                let i = self.sessions.len() - 1;
                self.hello(i);
                let mon = self.sessions[i].mon;
                let was_watching = self.sessions[..i].iter().any(|s| s.visible && s.mon == mon);
                self.flush_fresh(i, was_watching, now);
            }
            Cmd::Remove { id } => self.sessions.retain(|s| s.id != id),
            Cmd::Ack { id, seq } => {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    let now = Instant::now();
                    let s = &mut self.sessions[i];
                    // Acks can only move forward (seq wraps after 4 billion frames).
                    let mut unblocked = false;
                    if (seq.wrapping_sub(s.acked) as i32) > 0 && (s.seq.wrapping_sub(seq) as i32) >= 0 {
                        let was_blocked = s.in_flight() >= s.max_inflight;
                        s.acked = seq;
                        s.note_ack(seq, now);
                        unblocked = s.visible && was_blocked && s.in_flight() < s.max_inflight;
                        self.poll_capture |= unblocked;
                    }
                    // If this client was backed up (so capture was paused), let the
                    // loop poll live capture first rather than refining stale tiles.
                    if !unblocked && self.sessions[i].tiles.dirty.contains(&true) {
                        self.flush(i, now);
                    }
                }
            }
            Cmd::Visible { id, visible } => {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    let mon = self.sessions[i].mon;
                    let was_watching = self.sessions.iter().enumerate().any(|(j, s)| j != i && s.visible && s.mon == mon);
                    let was_visible = self.sessions[i].visible;
                    let s = &mut self.sessions[i];
                    s.visible = visible;
                    // Changes from while it was asleep are not latency.
                    s.pending_since = None;
                    if visible && !was_visible {
                        self.flush_fresh(i, was_watching, Instant::now());
                    }
                }
            }
            Cmd::Configure { id, change } => {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    let prev_mon = self.sessions[i].mon;
                    let was_visible = self.sessions[i].visible;
                    let v = &mut self.sessions[i].view;
                    let before = *v;
                    v.mon = change.mon.unwrap_or(v.mon);
                    v.quality = change.quality.unwrap_or(v.quality);
                    v.scale = change.scale.unwrap_or(v.scale);
                    v.fps = change.fps.unwrap_or(v.fps);
                    v.soft = change.soft.unwrap_or(v.soft);
                    v.lossless = change.lossless.unwrap_or(v.lossless);
                    let resend = |v: &View| (v.mon, v.quality, v.scale, v.lossless);
                    if resend(v) != resend(&before) {
                        self.sessions[i].quality_backoff = 0;
                        self.sessions[i].clear_since = None;
                        self.hello(i); // clamps the monitor index and forces a full resend
                        let mon = self.sessions[i].mon;
                        let was_watching = (was_visible && mon == prev_mon)
                            || self.sessions.iter().enumerate().any(|(j, s)| j != i && s.visible && s.mon == mon);
                        self.flush_fresh(i, was_watching, Instant::now());
                    }
                }
            }
        }
    }

    /// Capture monitors with a session ready to receive new pixels.
    fn capture(&mut self, timeout: Duration, now: Instant) -> bool {
        self.wanted.clear();
        self.wanted.extend(self.sessions.iter().filter(|s| s.can_capture(now)).map(|s| s.mon));
        self.wanted.sort_unstable();
        self.wanted.dedup();

        let mut captured_frame = false;
        for (n, &mon) in self.wanted.iter().enumerate() {
            let timeout = if n == 0 { timeout } else { Duration::ZERO };
            let screen = self.screens.entry(mon).or_default();
            match self.cap.acquire(mon, timeout, |px| screen.update(px)) {
                Ok(true) => {
                    let now = Instant::now();
                    captured_frame = true;
                    self.stats.capture += self.cap.last_cost;
                    // The image is as of just before it was copied off the GPU.
                    let captured = now.checked_sub(self.cap.last_cost).unwrap_or(now);
                    self.last_capture = Some(captured);
                    if self.failing {
                        self.failing = false;
                        crate::log!("capture recovered");
                    }
                    if screen.changed.contains(&true) {
                        for s in self.sessions.iter_mut().filter(|s| s.mon == mon) {
                            if s.dims == (screen.width, screen.height) {
                                for (d, c) in s.tiles.dirty.iter_mut().zip(&screen.changed) {
                                    *d |= *c;
                                }
                            }
                            if s.visible {
                                s.pending_since.get_or_insert(captured);
                            }
                        }
                    }
                }
                Ok(false) => {}
                Err(err) => {
                    if !self.failing {
                        self.failing = true;
                        crate::log!("capture unavailable ({err}); retrying until it comes back");
                    }
                    self.cap.refresh();
                    // A new layout can be captured immediately. Back off if
                    // re-enumerating found no change or no available display.
                    if !self.publish_monitors() {
                        self.retry_at = Some(Instant::now() + RETRY);
                    }
                    break;
                }
            }
        }
        captured_frame
    }

    fn poll_input(&mut self, now: Instant, captured: bool) {
        #[cfg(windows)]
        {
            if !captured && self.input_poll_at.is_some_and(|at| at > now) {
                return;
            }
            self.input_poll_at = Some(now + INPUT_POLL);
        }
        #[cfg(target_os = "linux")]
        let _ = (now, captured);
        let cursor = platform::cursor_name();
        let blocked = platform::input_blocked();
        for i in 0..self.sessions.len() {
            let s = &mut self.sessions[i];
            if !s.visible {
                continue;
            }
            if s.cursor != cursor {
                s.cursor = cursor;
                s.send_text(serde_json::json!({ "t": "cur", "c": cursor }).to_string());
            }
            if s.blocked != blocked {
                s.blocked = blocked;
                s.send_text(serde_json::json!({ "t": "blocked", "v": blocked }).to_string());
            }
        }
    }

    fn flush_all(&mut self, captured: bool) {
        self.poll_input(Instant::now(), captured);
        for i in 0..self.sessions.len() {
            self.flush(i, Instant::now());
        }
        self.report(Instant::now());
    }

    /// Send session `index` whatever it is owed, if it has room for a frame.
    fn flush(&mut self, index: usize, now: Instant) {
        if self.flush_once(index, now) == Some(Pass::Refine)
            && self.sessions[index].tiles.dirty.contains(&true)
        {
            self.flush_once(index, Instant::now());
        }
    }

    fn flush_once(&mut self, index: usize, now: Instant) -> Option<Pass> {
        let s = &mut self.sessions[index];
        let screen = self.screens.get(&s.mon)?;
        if !s.visible || screen.width == 0 {
            return None;
        }
        // A whole new picture (just connected, or switched monitor, quality or
        // resolution) goes out sharp straight away rather than blurry and
        // then again sharp.
        if let Some(at) = s.send_ready_at() {
            if at > now {
                return None;
            }
            s.acked = s.seq;
        }
        let fresh = s.dims != (screen.width, screen.height);
        if fresh {
            s.dims = (screen.width, screen.height);
            s.tiles.reset(screen.cols * screen.rows, now);
        }

        let moving_ready = s.moving_ready_at().is_none_or(|at| at <= now);
        let refine_ready = s.in_flight() == 0 && s.large_motion_hold_until().is_none_or(|at| at <= now);
        let refine_after = s.refine_after();
        let (tiles, pass) = s.tiles.next(now, fresh, moving_ready, refine_ready, refine_after)?;
        let dirty_count = tiles.iter().filter(|&&t| t).count();
        let total_tiles = tiles.len();
        // Tiny updates (typing, menus, hovers) stay crisp immediately even in
        // soft mode, while large motion (video, full-screen scroll) skips WebP
        // during the moving pass and refines losslessly once still.
        let small_update = dirty_count <= 16 && dirty_count * 20 <= total_tiles;
        let large_motion = dirty_count * 4 > total_tiles;
        let sharp = || {
            let (quality, full_chroma) = s.view.quality.still();
            (quality, FLAG_REFINE | if full_chroma { FLAG_FULL_CHROMA } else { 0 })
        };
        let ((quality, mut flags), since) = match pass {
            Pass::Fresh => (sharp(), s.pending_since.take()),
            Pass::Refine => (sharp(), None),
            Pass::Moving => {
                let flags = if s.view.soft && !small_update { FLAG_SOFT } else { 0 };
                ((s.view.quality.backoff(s.quality_backoff).moving(), flags), s.pending_since.take())
            }
        };
        // Soft motion is the cheaper way to send what keeps changing; the
        // lossless re-send follows once it stops. Large non-soft motion also
        // uses JPEG while moving and refines losslessly once still.
        if s.view.lossless && flags & FLAG_SOFT == 0 && !(pass == Pass::Moving && large_motion) {
            flags |= FLAG_LOSSLESS;
        }

        s.seq = s.seq.wrapping_add(1);
        let started = Instant::now();
        let frame = frame_message(screen, &tiles, s.view.scale, quality, flags, s.seq);
        s.tiles.sent(&frame.drawn, &frame.exact, pass == Pass::Moving, now);
        let mut msg = frame.msg;
        let done = Instant::now();
        let host = done.duration_since(since.unwrap_or(started).min(started));
        msg[11..15].copy_from_slice(&(host.as_micros().min(u32::MAX as u128) as u32).to_le_bytes());
        self.stats.encode += done - started;
        self.stats.frames += 1;
        self.stats.bytes += msg.len();
        s.last_send = done;
        s.record_sent(s.seq, done, pass == Pass::Moving && msg.len() >= LARGE_FRAME_BYTES);
        if pass == Pass::Moving {
            s.last_moving = Some(now);
            if dirty_count * 2 > total_tiles {
                s.last_large_motion = Some(now);
            }
        }
        (s.tx)(Out::Binary(msg));
        Some(pass)
    }

    /// With --verbose, summarise throughput every couple of seconds.
    fn report(&mut self, now: Instant) {
        if !self.shared.verbose {
            return;
        }
        let since = *self.stats.since.get_or_insert(now);
        let elapsed = now.duration_since(since);
        if elapsed < Duration::from_secs(2) {
            return;
        }
        if self.stats.frames > 0 {
            let n = self.stats.frames as f64;
            crate::log!(
                "stats: {:.1} frames/s, {:.0} KB/s, capture {:.2} ms, encode {:.2} ms per frame",
                n / elapsed.as_secs_f64(),
                self.stats.bytes as f64 / 1024.0 / elapsed.as_secs_f64(),
                self.stats.capture.as_secs_f64() * 1000.0 / n,
                self.stats.encode.as_secs_f64() * 1000.0 / n,
            );
        }
        self.stats = Stats { since: Some(now), ..Stats::default() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(width: usize, height: usize) -> Screen {
        let mut s = Screen::default();
        let data = vec![0u8; width * height * 4];
        s.update(Pixels::bgra(width, height, width * 4, &data));
        s
    }

    fn frame(height: usize, stride: usize, paint: &[(usize, usize)]) -> Vec<u8> {
        let mut data = vec![0u8; stride * height];
        for &(x, y) in paint {
            data[y * stride + x * 4] = 200;
        }
        data
    }

    #[test]
    fn first_frame_marks_everything() {
        let s = screen(200, 100);
        assert_eq!((s.cols, s.rows), (4, 2));
        assert!(s.changed.iter().all(|&c| c));
    }

    #[test]
    fn update_flags_only_touched_tiles_and_honours_stride() {
        let mut s = screen(200, 100);
        let stride = 200 * 4 + 32; // padded rows, as GPUs like to hand out
        let data = frame(100, stride, &[(70, 10), (199, 99)]);
        s.update(Pixels::bgra(200, 100, stride, &data));
        let flagged: Vec<usize> = (0..8).filter(|&i| s.changed[i]).collect();
        assert_eq!(flagged, vec![1, 7]); // tile (1,0) and the partial tile (3,1)
        s.update(Pixels::bgra(200, 100, stride, &data));
        assert!(s.changed.iter().all(|&c| !c), "identical frame changes nothing");
        assert_eq!(s.buf[10 * 800 + 70 * 4], 200);
    }

    #[test]
    fn full_diff_normalizes_layout_and_ignores_alpha() {
        let (width, height) = (70, TILE * 4 + 1);
        let mut s = painted(width, height, |x, y| [x as u8, y as u8, 123]);
        let stride = width * 4 + 8;
        let mut data = vec![0; stride * height];
        for y in 0..height {
            for x in 0..width {
                let at = (height - 1 - y) * stride + x * 4;
                data[at..at + 4].copy_from_slice(&[123, y as u8, x as u8, 255]);
            }
        }
        fn pixels(data: &[u8], width: usize, height: usize, stride: usize) -> Pixels<'_> {
            Pixels { y_invert: true, swap_rb: true, ..Pixels::bgra(width, height, stride, data) }
        }
        s.update(pixels(&data, width, height, stride));
        assert!(!s.changed.contains(&true));

        data[66 * 4] = 42;
        s.update(pixels(&data, width, height, stride));
        assert_eq!(s.changed.iter().position(|&changed| changed), Some(9));
        assert_eq!(s.changed.iter().filter(|&&changed| changed).count(), 1);
        let at = ((height - 1) * width + 66) * 4;
        assert_eq!(&s.buf[at..at + 3], &[66, 0, 42]);

        let mut fresh = Screen::default();
        fresh.update(pixels(&data, width, height, stride));
        assert!(fresh.changed.iter().all(|&changed| changed));
        assert_eq!(&fresh.buf[at..at + 3], &[66, 0, 42]);
    }

    #[test]
    fn damage_reads_only_reported_pixels_and_marks_intersecting_tiles() {
        use crate::platform::DamageRect;

        let mut s = screen(130, 65);
        let stride = s.width * 4;
        let data = frame(s.height, stride, &[(62, 63), (63, 63), (64, 64), (129, 64)]);
        let damage = [
            DamageRect { x: 63, y: 63, w: 2, h: 2 },
            DamageRect { x: 129, y: 64, w: 20, h: 20 },
            DamageRect { x: 200, y: 100, w: 20, h: 20 },
        ];
        s.update(Pixels { damage: Some(&damage), ..Pixels::bgra(130, 65, stride, &data) });
        assert_eq!(s.changed, [true, false, false, false, true, true]);
        assert_eq!(s.buf[63 * stride + 62 * 4], 0);
        assert_eq!(s.buf[63 * stride + 63 * 4], 200);

        let mut alpha = s.buf.clone();
        for px in alpha.chunks_exact_mut(4) {
            px[3] = 255;
        }
        s.update(Pixels::bgra(130, 65, stride, &alpha));
        assert!(!s.changed.contains(&true));
    }

    #[test]
    fn merge_stacks_equal_runs_and_clips_to_the_screen() {
        // 4x3 tiles on a 200x150 screen (last column 8 px wide, last row 22 px high)
        #[rustfmt::skip]
        let tiles = [
            true,  true,  false, false,
            true,  true,  false, true,
            false, false, false, true,
        ];
        let mut rects = merge(&tiles, 4, 3, 200, 150);
        rects.sort_by_key(|r| (r.y, r.x));
        assert_eq!(
            rects,
            vec![
                Rect { x: 0, y: 0, w: 128, h: 128 },
                Rect { x: 192, y: 64, w: 8, h: 86 },
            ]
        );
    }

    #[test]
    fn merge_whole_screen_is_one_rect() {
        assert_eq!(merge(&[true; 12], 4, 3, 200, 150), vec![Rect { x: 0, y: 0, w: 200, h: 150 }]);
        assert!(merge(&[false; 12], 4, 3, 200, 150).is_empty());
    }

    #[test]
    fn merge_falls_back_to_coarse_rects_when_scattered() {
        // checkerboard: far more than MAX_RECTS separate tiles
        let (cols, rows) = (40, 20);
        let tiles: Vec<bool> = (0..cols * rows).map(|i| (i % cols + i / cols) % 2 == 0).collect();
        let rects = merge(&tiles, cols, rows, cols * TILE, rows * TILE);
        assert!(rects.len() <= MAX_RECTS, "{} rects", rects.len());
        for (i, &t) in tiles.iter().enumerate() {
            let (x, y) = ((i % cols) * TILE, (i / cols) * TILE);
            let covered = rects.iter().any(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h);
            assert!(!t || covered, "dirty tile {i} not covered");
        }
    }

    #[test]
    fn frame_message_layout() {
        let mut s = screen(200, 150);
        let data = frame(150, 800, &[(5, 5)]);
        s.update(Pixels::bgra(200, 150, 800, &data));
        let Frame { msg, exact, .. } = frame_message(&s, &s.changed, 1, 70, FLAG_REFINE, 7);
        assert!(exact.is_empty(), "nothing is lossless without the flag");
        assert_eq!(msg[0], 1);
        assert_eq!(u32::from_le_bytes(msg[1..5].try_into().unwrap()), 7);
        assert_eq!(u16::from_le_bytes(msg[5..7].try_into().unwrap()), 200);
        assert_eq!(u16::from_le_bytes(msg[7..9].try_into().unwrap()), 150);
        assert_eq!(u16::from_le_bytes(msg[9..11].try_into().unwrap()), 1);
        assert_eq!(&msg[11..15], &[0, 0, 0, 0], "host time is left for the caller");
        assert_eq!((msg[15], msg[16]), (70, FLAG_REFINE));
        let at = |m: &[u8], i: usize| u16::from_le_bytes(m[HEADER + i * 2..HEADER + i * 2 + 2].try_into().unwrap());
        assert_eq!((0..4).map(|i| at(&msg, i)).collect::<Vec<_>>(), vec![0, 0, 64, 64]);
        let len = u32::from_le_bytes(msg[HEADER + 8..HEADER + 12].try_into().unwrap()) as usize;
        assert_eq!(msg.len(), HEADER + 12 + len);
        assert_eq!(&msg[HEADER + 12..HEADER + 14], &[0xFF, 0xD8], "JPEG start marker");

        assert_eq!(jpeg_size(&msg[HEADER + 12..]), (64, 64));

        // half resolution: same tile, half the size
        let half = frame_message(&s, &s.changed, 2, 70, 0, 8).msg;
        assert_eq!(u16::from_le_bytes(half[5..7].try_into().unwrap()), 100);
        assert_eq!((0..4).map(|i| at(&half, i)).collect::<Vec<_>>(), vec![0, 0, 32, 32]);
        assert_eq!(jpeg_size(&half[HEADER + 12..]), (32, 32));

        // soft: drawn at full size, encoded at half
        let soft = frame_message(&s, &s.changed, 1, 70, FLAG_SOFT, 9).msg;
        assert_eq!((0..4).map(|i| at(&soft, i)).collect::<Vec<_>>(), vec![0, 0, 64, 64]);
        assert_eq!(jpeg_size(&soft[HEADER + 12..]), (32, 32));

        // soft at half resolution: drawn at half, encoded at a quarter
        let both = frame_message(&s, &s.changed, 2, 70, FLAG_SOFT, 10).msg;
        assert_eq!((0..4).map(|i| at(&both, i)).collect::<Vec<_>>(), vec![0, 0, 32, 32]);
        assert_eq!(jpeg_size(&both[HEADER + 12..]), (16, 16));
    }

    /// Size and encode time of a full-screen frame of the real desktop at
    /// each setting. Run with `cargo test --release -- --ignored --nocapture frame_sizes`.
    #[test]
    #[ignore]
    fn frame_sizes() {
        platform::init();
        let mut cap = Capture::new();
        let mut s = Screen::default();
        assert!(cap.acquire(0, Duration::ZERO, |px| s.update(px)).unwrap());
        let all = vec![true; s.cols * s.rows];
        println!("{}x{} screen, whole screen changed:", s.width, s.height);
        let heads = ["moving", "moving, soft", "sharp", "moving, lossless", "sharp, lossless"];
        println!("{:<8} {:<5} {}", "quality", "res", heads.map(|h| format!("{h:>18}")).join(""));
        for q in [Quality::Lowest, Quality::Low, Quality::Med, Quality::High] {
            for scale in [1, 2] {
                let mut cells = Vec::new();
                let (still_q, chroma) = q.still();
                let still_flags = FLAG_REFINE | if chroma { FLAG_FULL_CHROMA } else { 0 };
                for (quality, flags) in [
                    (q.moving(), 0),
                    (q.moving(), FLAG_SOFT),
                    (still_q, still_flags),
                    (q.moving(), FLAG_LOSSLESS),
                    (still_q, still_flags | FLAG_LOSSLESS),
                ] {
                    let t = Instant::now();
                    let n = frame_message(&s, &all, scale, quality, flags, 1).msg.len();
                    cells.push(format!("{:>9.0} KB {:>4.1} ms", n as f64 / 1024.0, t.elapsed().as_secs_f64() * 1e3));
                }
                let res = if scale == 1 { "full" } else { "half" };
                println!("{:<8} {:<5} {}", format!("{q:?}"), res, cells.concat());
            }
        }
    }

    #[test]
    fn settled_tiles_are_sharpened_while_others_keep_moving() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let on = |v: &[bool]| (0..v.len()).filter(|&i| v[i]).collect::<Vec<_>>();
        let mut tiles = Tiles::default();
        tiles.reset(4, t0);

        let (sent, pass) = tiles.next(at(0), true, true, true, REFINE_AFTER).unwrap();
        assert_eq!((on(&sent), pass), (vec![0, 1, 2, 3], Pass::Fresh));
        assert!(tiles.next(at(1), false, true, true, REFINE_AFTER).is_none(), "a fresh picture is already sharp");

        // A scroll changes everything, then a spinner keeps animating tile 3.
        tiles.dirty = vec![true; 4];
        let (sent, pass) = tiles.next(at(10), false, true, true, REFINE_AFTER).unwrap();
        assert_eq!((on(&sent), pass), (vec![0, 1, 2, 3], Pass::Moving));
        for ms in (30..100).step_by(20) {
            tiles.dirty[3] = true;
            let (sent, pass) = tiles.next(at(ms), false, true, true, REFINE_AFTER).unwrap();
            assert_eq!((on(&sent), pass), (vec![3], Pass::Moving), "at {ms} ms");
        }
        // While moving is ready, live motion takes priority over refining.
        tiles.dirty[3] = true;
        let (sent, pass) = tiles.next(at(120), false, true, true, REFINE_AFTER).unwrap();
        assert_eq!((on(&sent), pass), (vec![3], Pass::Moving));
        // Once moving is not pending (or between frames) and the pipe is idle,
        // settled tiles 0-2 sharpen even while tile 3 keeps moving.
        tiles.dirty[3] = true;
        assert!(tiles.next(at(120), false, false, false, REFINE_AFTER).is_none(), "no refine while frames are in flight");
        let (sent, pass) = tiles.next(at(120), false, false, true, REFINE_AFTER).unwrap();
        assert_eq!((on(&sent), pass), (vec![0, 1, 2], Pass::Refine));
        // The spinner's update waits for its moving-frame deadline.
        assert!(tiles.next(at(120), false, false, true, REFINE_AFTER).is_none());
        let (sent, pass) = tiles.next(at(121), false, true, true, REFINE_AFTER).unwrap();
        assert_eq!((on(&sent), pass), (vec![3], Pass::Moving));

        // Once the spinner stops, its tile is sharpened after REFINE_AFTER too.
        assert!(tiles.next(at(180), false, true, true, REFINE_AFTER).is_none());
        let (sent, pass) = tiles.next(at(230), false, true, true, REFINE_AFTER).unwrap();
        assert_eq!((on(&sent), pass), (vec![3], Pass::Refine));
        assert!(tiles.next(at(500), false, true, true, REFINE_AFTER).is_none());
    }

    #[test]
    fn tiles_a_whole_row_merge_draws_blurry_get_sharpened_too() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let s = screen(1920, 256);
        let mut tiles = Tiles::default();
        tiles.reset(s.cols * s.rows, t0);
        tiles.next(at(0), true, true, true, REFINE_AFTER);
        // A checkerboard of changed tiles: coarse + coalesced merging covers
        // the whole strip as one rectangle instead of 4 separate strips.
        let (col, row) = (|t: usize| t % s.cols, |t: usize| t / s.cols);
        tiles.dirty = (0..s.cols * s.rows).map(|t| (col(t) + row(t)) % 2 == 0).collect();
        let (sent, pass) = tiles.next(at(10), false, true, true, REFINE_AFTER).unwrap();
        assert_eq!(pass, Pass::Moving);
        let frame = frame_message(&s, &sent, 1, 70, 0, 1);
        let drawn = vec![true; s.cols * s.rows];
        assert_eq!(frame.drawn, drawn);
        tiles.sent(&frame.drawn, &frame.exact, true, at(10));
        // Once still and idle, everything drawn blurry is re-sent sharp.
        assert_eq!(tiles.next(at(150), false, true, true, REFINE_AFTER), Some((drawn, Pass::Refine)));
    }

    #[test]
    fn coalesce_merges_fragmented_video_and_keeps_distant_regions_separate() {
        // 30x17 grid (1920x1080): ragged video rows with small static gaps
        // should collapse to one rectangle.
        let (cols, rows) = (30, 17);
        let mut video = vec![true; cols * rows];
        for r in 0..rows {
            // Vary left/right edge and leave a 1-tile static hole per row.
            if r % 2 == 0 {
                video[r * cols] = false;
            }
            if r % 3 == 0 {
                video[r * cols + cols - 1] = false;
            }
            video[r * cols + (r * 3 + 5) % (cols - 2) + 1] = false;
        }
        let raw = merge(&video, cols, rows, 1920, 1080);
        assert!(raw.len() >= 15, "uncoalesced video fragments into many strips (got {})", raw.len());
        let merged = merge_with_coalesce(&video, cols, rows, 1920, 1080, true);
        assert_eq!(merged, vec![Rect { x: 0, y: 0, w: 1920, h: 1080 }]);

        // Two distant small regions (top-left cursor + bottom-right clock) stay separate.
        let mut sparse = vec![false; cols * rows];
        sparse[0] = true;
        sparse[rows * cols - 1] = true;
        let sparse_merged = merge_with_coalesce(&sparse, cols, rows, 1920, 1080, true);
        assert_eq!(sparse_merged.len(), 2);
    }

    #[test]
    fn inflight_cap_and_quality_backoff_use_excess_delay_and_recover() {
        let t0 = Instant::now();
        let mut s = Session {
            id: 1,
            tx: Arc::new(|_| {}),
            view: View { mon: 0, quality: Quality::Med, scale: 1, fps: 60, soft: true, lossless: true },
            mon: 0,
            visible: true,
            dims: (0, 0),
            tiles: Tiles::default(),
            pending_since: None,
            seq: 1,
            acked: 0,
            max_inflight: MAX_INFLIGHT,
            min_rtt: None,
            slow_acks: 0,
            quality_backoff: 0,
            clear_since: None,
            sent_at: [None; 4],
            last_send: t0,
            last_moving: None,
            last_large_motion: None,
            cursor: "",
            blocked: false,
        };
        assert_eq!(s.refine_after(), Duration::from_millis(100));
        s.view.fps = 10;
        assert_eq!(s.refine_after(), Duration::from_millis(200));

        // Establish a 20 ms baseline RTT.
        s.record_sent(1, t0, false);
        s.note_ack(1, t0 + Duration::from_millis(20));
        assert_eq!(s.min_rtt, Some(Duration::from_millis(20)));
        assert_eq!(s.max_inflight, MAX_INFLIGHT);
        assert_eq!(s.quality_backoff, 0);

        // One congested blip does not shrink the pipeline or quality.
        s.record_sent(2, t0 + Duration::from_millis(100), true);
        s.note_ack(2, t0 + Duration::from_millis(250)); // 150 ms RTT, 130 ms excess
        assert_eq!(s.max_inflight, MAX_INFLIGHT);
        assert_eq!(s.quality_backoff, 0);

        // Second consecutive congested large moving frame shrinks in-flight to 1
        // and steps moving quality down one level (Med -> Low).
        s.record_sent(3, t0 + Duration::from_millis(300), true);
        s.note_ack(3, t0 + Duration::from_millis(450));
        assert_eq!(s.max_inflight, 1);
        assert_eq!(s.quality_backoff, 1);
        assert_eq!(s.view.quality.backoff(s.quality_backoff), Quality::Low);

        // Two more congested large frames step down again (Low -> Lowest), clamped at Lowest.
        s.record_sent(4, t0 + Duration::from_millis(500), true);
        s.note_ack(4, t0 + Duration::from_millis(650));
        s.record_sent(5, t0 + Duration::from_millis(700), true);
        s.note_ack(5, t0 + Duration::from_millis(850));
        assert_eq!(s.quality_backoff, 2);
        assert_eq!(s.view.quality.backoff(s.quality_backoff), Quality::Lowest);

        // Hysteresis band (50..100 ms excess) holds steady without pumping.
        s.record_sent(6, t0 + Duration::from_millis(900), true);
        s.note_ack(6, t0 + Duration::from_millis(995)); // 95 ms RTT, 75 ms excess
        assert_eq!(s.max_inflight, 1);
        assert_eq!(s.quality_backoff, 2);

        // Clean ack restores full pipeline immediately and starts quality recovery timer.
        s.record_sent(7, t0 + Duration::from_millis(1000), true);
        s.note_ack(7, t0 + Duration::from_millis(1040)); // 40 ms RTT, 20 ms excess
        assert_eq!(s.max_inflight, MAX_INFLIGHT);
        assert_eq!(s.quality_backoff, 2);

        // Sustained clean acks step moving quality back up after BACKOFF_RECOVER.
        s.record_sent(8, t0 + Duration::from_millis(2600), true);
        s.note_ack(8, t0 + Duration::from_millis(2640));
        assert_eq!(s.quality_backoff, 1);
        s.record_sent(9, t0 + Duration::from_millis(4200), true);
        s.note_ack(9, t0 + Duration::from_millis(4240));
        assert_eq!(s.quality_backoff, 0);

        // Fast high-RTT long-distance link (180 ms baseline, 200 ms under load -> 20 ms excess)
        // must never trigger congestion backoff.
        let mut far = Session {
            id: 2,
            tx: Arc::new(|_| {}),
            view: View { mon: 0, quality: Quality::High, scale: 1, fps: 60, soft: false, lossless: true },
            mon: 0,
            visible: true,
            dims: (0, 0),
            tiles: Tiles::default(),
            pending_since: None,
            seq: 1,
            acked: 0,
            max_inflight: MAX_INFLIGHT,
            min_rtt: None,
            slow_acks: 0,
            quality_backoff: 0,
            clear_since: None,
            sent_at: [None; 4],
            last_send: t0,
            last_moving: None,
            last_large_motion: None,
            cursor: "",
            blocked: false,
        };
        far.record_sent(1, t0, false);
        far.note_ack(1, t0 + Duration::from_millis(180));
        for seq in 2..=6 {
            let start = t0 + Duration::from_millis(seq as u64 * 250);
            far.record_sent(seq, start, true);
            far.note_ack(seq, start + Duration::from_millis(200));
        }
        assert_eq!(far.max_inflight, MAX_INFLIGHT);
        assert_eq!(far.quality_backoff, 0);
    }

    #[test]
    fn capture_rate_follows_the_fastest_client_within_the_host_limit() {
        let host = Duration::from_secs_f64(1.0 / 60.0);
        let ms = |d: Duration| (d.as_secs_f64() * 1000.0).round() as u32;
        assert_eq!(ms(capture_interval(host, [].into_iter())), 17);
        assert_eq!(ms(capture_interval(host, [10].into_iter())), 100);
        assert_eq!(ms(capture_interval(host, [10, 30].into_iter())), 33);
        assert_eq!(ms(capture_interval(host, [10, 0].into_iter())), 17, "0 means uncapped");
        assert_eq!(ms(capture_interval(host, [240].into_iter())), 17, "never above the host limit");
    }

    /// Width and height from a baseline JPEG's start-of-frame segment.
    fn jpeg_size(jpeg: &[u8]) -> (u16, u16) {
        let at = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0 marker");
        let h = u16::from_be_bytes([jpeg[at + 5], jpeg[at + 6]]);
        let w = u16::from_be_bytes([jpeg[at + 7], jpeg[at + 8]]);
        (w, h)
    }

    #[test]
    fn downscale_averages_blocks_and_partial_edges() {
        // 5x3 screen; blue channel = x * 10 + y, others 0.
        let (w, h) = (5, 3);
        let mut data = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                data[(y * w + x) * 4] = (x * 10 + y) as u8;
            }
        }
        let mut s = Screen::default();
        s.update(Pixels::bgra(w, h, w * 4, &data));
        let all = Rect { x: 0, y: 0, w, h };

        let (out, ow, oh) = downscale(&s, all, 2);
        assert_eq!((ow, oh), (3, 2));
        let blue: Vec<u8> = out.chunks(4).map(|p| p[0]).collect();
        // (0+1+10+11)/4 = 5.5 -> 6; (20+21+30+31)/4 = 25.5 -> 26; edge column x=4: (40+41)/2 = 40.5 -> 41;
        // bottom row y=2 alone: (2+12)/2 = 7, (22+32)/2 = 27, 42.
        assert_eq!(blue, vec![6, 26, 41, 7, 27, 42]);
        assert!(out.chunks(4).all(|p| p[1] == 0 && p[2] == 0 && p[3] == 255));

        let (out, ow, oh) = downscale(&s, all, 4);
        assert_eq!((ow, oh), (2, 1));
        // first block: x 0..4, y 0..3 -> mean of x*10+y = 15 + 1 = 16; second: x = 4 -> 40 + 1 = 41
        assert_eq!(out.chunks(4).map(|p| p[0]).collect::<Vec<_>>(), vec![16, 41]);
    }

    /// A BGRA screen whose pixels come from `paint(x, y)`, with the 4th byte
    /// left as junk the way capture may hand it over.
    fn painted(width: usize, height: usize, paint: impl Fn(usize, usize) -> [u8; 3]) -> Screen {
        let mut data = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let [b, g, r] = paint(x, y);
                data.extend_from_slice(&[b, g, r, 7]);
            }
        }
        let mut s = Screen::default();
        s.update(Pixels::bgra(width, height, width * 4, &data));
        s
    }

    /// Noise: every pixel a different colour from its neighbours.
    fn noise(x: usize, y: usize) -> [u8; 3] {
        let v = ((x * 7919 + y * 104_729) as u32).wrapping_mul(2_654_435_761);
        [(v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
    }

    /// (x, y, w, h, format, image bytes) of each rectangle of a lossless frame.
    fn parts(msg: &[u8]) -> Vec<(usize, usize, usize, usize, u8, &[u8])> {
        let u16at = |i: usize| u16::from_le_bytes([msg[i], msg[i + 1]]) as usize;
        let mut at = HEADER;
        let out = (0..u16at(9))
            .map(|_| {
                let len = u32::from_le_bytes(msg[at + 9..at + 13].try_into().unwrap()) as usize;
                let part = (u16at(at), u16at(at + 2), u16at(at + 4), u16at(at + 6), msg[at + 8], &msg[at + 13..at + 13 + len]);
                at += 13 + len;
                part
            })
            .collect();
        assert_eq!(at, msg.len());
        out
    }

    fn webp_rgba(data: &[u8]) -> (usize, usize, Vec<u8>) {
        let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(data)).unwrap();
        let (w, h) = d.dimensions();
        let mut buf = vec![0; d.output_buffer_size().unwrap()];
        d.read_image(&mut buf).unwrap();
        let rgba = if d.has_alpha() { buf } else { buf.chunks(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect() };
        (w as usize, h as usize, rgba)
    }

    #[test]
    fn lossless_frames_are_exact_and_leave_photos_to_jpeg() {
        // 3 x 2 tiles of text-like stripes, with noise (a "photo") in the top middle one.
        let stripes = |x: usize, y: usize| if (x / 3 + y / 5) % 4 == 0 { [20, 30, 40] } else { [250, 245, 240] };
        let s = painted(192, 100, |x, y| if (64..128).contains(&x) && y < 64 { noise(x, y) } else { stripes(x, y) });
        let flags = FLAG_REFINE | FLAG_FULL_CHROMA | FLAG_LOSSLESS;
        let Frame { msg, exact, .. } = frame_message(&s, &s.changed, 1, 90, flags, 3);
        assert_eq!(msg[16], flags);
        assert_eq!(exact, vec![true, false, true, true, true, true]);
        let p = parts(&msg);
        // The whole screen losslessly, then the photo tile as JPEG on top.
        assert_eq!(p.iter().map(|q| (q.0, q.1, q.2, q.3, q.4)).collect::<Vec<_>>(), vec![(0, 0, 192, 100, 1), (64, 0, 64, 64, 0)]);
        assert_eq!(&p[0].5[..4], b"RIFF");
        assert_eq!(&p[1].5[..2], &[0xFF, 0xD8]);
        let (w, h, rgba) = webp_rgba(p[0].5);
        assert_eq!((w, h), (192, 100));
        for y in 0..100 {
            for x in 0..192 {
                let got = &rgba[(y * 192 + x) * 4..][..4];
                let [b, g, r] = if (64..128).contains(&x) && y < 64 { [128, 128, 128] } else { stripes(x, y) };
                assert_eq!(got, [r, g, b, 255], "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn photo_jpegs_span_flat_tiles_only_within_what_is_sent() {
        // Photos in tiles 0 and 3, with the maximum flat gap between them.
        let s = painted(320, 64, |x, y| if matches!(x / TILE, 0 | 3) { noise(x, y) } else { [240; 3] });
        let jpegs = |tiles: &[bool]| {
            let Frame { msg, exact, .. } = frame_message(&s, tiles, 1, 70, FLAG_LOSSLESS, 6);
            let rects = parts(&msg).iter().filter(|p| p.4 == 0).map(|p| (p.0, p.2)).collect::<Vec<_>>();
            (rects, exact)
        };
        // All sent: one JPEG over tiles 0 to 3.
        assert_eq!(jpegs(&[true; 5]), (vec![(0, 256)], vec![false, false, false, false, true]));
        // Tile 1 not sent: it must not be drawn over.
        assert_eq!(jpegs(&[true, false, true, true, false]), (vec![(0, 64), (192, 64)], vec![false, false, true, false, false]));
    }

    #[test]
    fn wide_photo_gap_stays_lossless_and_webp_margins_are_trimmed() {
        let flat = [104, 150, 248];
        let s = painted(6 * TILE + 17, 33, |x, y| {
            if matches!(x / TILE, 0 | 6) { noise(x, y) } else { flat }
        });
        let Frame { msg, exact, .. } = frame_message(&s, &s.changed, 2, 70, FLAG_LOSSLESS, 7);
        assert_eq!(exact, [false, true, true, true, true, true, false]);
        let parts = parts(&msg);
        let rects: Vec<_> = parts.iter().map(|p| (p.0, p.1, p.2, p.3, p.4)).collect();
        assert_eq!(rects, [(32, 0, 160, 17, 1), (0, 0, 32, 17, 0), (192, 0, 9, 17, 0)]);
        let (w, h, rgba) = webp_rgba(parts[0].5);
        assert_eq!((w, h), (160, 17));
        assert!(rgba.chunks_exact(4).all(|p| p == [flat[2], flat[1], flat[0], 255]));
    }

    #[test]
    fn lossless_at_half_resolution_is_the_downscaled_image() {
        let s = painted(128, 64, |x, _| if x < 64 { [0, 0, 0] } else { [200, 100, 50] });
        let Frame { msg, exact, .. } = frame_message(&s, &s.changed, 2, 70, FLAG_LOSSLESS, 4);
        assert_eq!(exact, vec![true, true]);
        let p = parts(&msg);
        assert_eq!(p.iter().map(|q| (q.0, q.1, q.2, q.3, q.4)).collect::<Vec<_>>(), vec![(0, 0, 64, 32, 1)]);
        let (w, h, rgba) = webp_rgba(p[0].5);
        assert_eq!((w, h), (64, 32));
        assert_eq!((&rgba[..4], &rgba[63 * 4..64 * 4]), (&[0, 0, 0, 255][..], &[50, 100, 200, 255][..]));
    }

    #[test]
    fn soft_frames_stay_jpeg_and_flagless_frames_are_unchanged() {
        let s = painted(64, 64, |x, y| if (x + y) % 9 == 0 { [0, 0, 0] } else { [255, 255, 255] });
        let Frame { msg, exact, .. } = frame_message(&s, &s.changed, 1, 70, 0, 5);
        assert!(exact.is_empty());
        assert_eq!(&msg[HEADER + 12..HEADER + 14], &[0xFF, 0xD8], "no format byte without the flag");
    }

    #[test]
    fn text_and_flat_ui_look_flat_and_photos_do_not() {
        let flat = |s: &Screen| (0..s.cols * s.rows).map(|t| looks_flat(s, t)).collect::<Vec<_>>();
        // Fine vertical stripes: every pixel repeats the one above.
        assert_eq!(flat(&painted(64, 64, |x, _| [x as u8 % 2 * 255; 3])), vec![true]);
        // Noise, in a full tile and in a partial one at the screen's edge.
        assert_eq!(flat(&painted(100, 64, noise)), vec![false, false]);
        // ClearType-like: 513 colours, all on a few lines, background elsewhere.
        assert_eq!(flat(&painted(64, 64, |x, y| if y % 16 < 2 { [x as u8, y as u8, 0] } else { [255; 3] })), vec![true]);
        // A dark gradient dithered with four levels of noise: few colours, but no text.
        assert_eq!(flat(&painted(64, 64, |x, y| [20, 10, 40 + (noise(x, y)[0] % 4)])), vec![false]);
    }
}
