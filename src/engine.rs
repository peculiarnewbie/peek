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

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use rayon::prelude::*;

use crate::fake::Source;
use crate::platform::{self, Pixels};
use crate::screen::{Shared, Sink};
use crate::webp;

/// Tile edge in pixels. A multiple of 16 so JPEG blocks line up at the seams.
pub const TILE: usize = 64;
/// Frames sent but not yet drawn by the client. Bounds latency on slow links.
const MAX_INFLIGHT: u32 = 2;
/// Stillness needed before blurry regions are re-sent sharply.
const REFINE_AFTER: Duration = Duration::from_millis(250);
/// Longest single wait for the screen to change; bounds how long a command
/// (an ack, a new client) can sit unprocessed.
const ACQUIRE_WAIT: Duration = Duration::from_millis(10);
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
    /// A frame, with when the engine handed it over (`screen::now_us`), so
    /// whoever writes it to the socket can say how long the way there took.
    Binary(Vec<u8>, u64),
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
        let row_bytes = px.width * 4;
        if (px.width, px.height) != (self.width, self.height) {
            self.width = px.width;
            self.height = px.height;
            self.cols = px.width.div_ceil(TILE);
            self.rows = px.height.div_ceil(TILE);
            self.buf = vec![0; row_bytes * px.height];
            for y in 0..px.height {
                self.buf[y * row_bytes..][..row_bytes]
                    .copy_from_slice(&px.data[y * px.stride..][..row_bytes]);
            }
            self.changed = vec![true; self.cols * self.rows];
            return;
        }
        self.changed.fill(false);
        let tile_bytes = TILE * 4;
        for y in 0..px.height {
            let src = &px.data[y * px.stride..][..row_bytes];
            let dst = &mut self.buf[y * row_bytes..][..row_bytes];
            let flags = &mut self.changed[(y / TILE) * self.cols..][..self.cols];
            for ((s, d), flag) in
                src.chunks(tile_bytes).zip(dst.chunks_mut(tile_bytes)).zip(flags.iter_mut())
            {
                if s != d {
                    d.copy_from_slice(s);
                    *flag = true;
                }
            }
        }
    }
}

struct Session {
    id: u64,
    tx: Sink,
    view: View,
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
    last_send: Instant,
    cursor: &'static str,
    blocked: bool,
}

impl Session {
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

    /// Pick the next frame's tiles. Stillness is judged per tile, so a small
    /// animation somewhere does not keep the rest of the screen blurry.
    fn next(&mut self, now: Instant, fresh: bool) -> Option<(Vec<bool>, Pass)> {
        let blank = vec![false; self.dirty.len()];
        if fresh {
            self.lossy.fill(false);
            return Some((std::mem::replace(&mut self.dirty, blank), Pass::Fresh));
        }
        // Settled tiles go first: a one-off frame, and the moving regions
        // are only a frame behind.
        let settled: Vec<bool> = (0..self.dirty.len())
            .map(|t| self.lossy[t] && !self.dirty[t] && now.duration_since(self.changed_at[t]) >= REFINE_AFTER)
            .collect();
        if settled.iter().any(|&t| t) {
            for (l, &t) in self.lossy.iter_mut().zip(&settled) {
                *l &= !t;
            }
            return Some((settled, Pass::Refine));
        }
        if self.dirty.iter().any(|&d| d) {
            for t in 0..self.dirty.len() {
                if self.dirty[t] {
                    self.lossy[t] = true;
                    self.changed_at[t] = now;
                }
            }
            return Some((std::mem::replace(&mut self.dirty, blank), Pass::Moving));
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

/// Merge flagged tiles into rectangles: horizontal runs per tile row, then
/// runs with identical extents on consecutive rows are stacked.
fn merge(tiles: &[bool], cols: usize, rows: usize, width: usize, height: usize) -> Vec<Rect> {
    let rects = stack(tiles, cols, rows, width, height, false);
    if rects.len() > MAX_RECTS { stack(tiles, cols, rows, width, height, true) } else { rects }
}

/// Rectangles of flagged tiles: runs per tile row (`coarse`: one run from the
/// first flagged tile to the last), stacked while consecutive rows match.
fn stack(tiles: &[bool], cols: usize, rows: usize, width: usize, height: usize, coarse: bool) -> Vec<Rect> {
    let mut done: Vec<Rect> = Vec::new();
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
                let (x, y) = (c0 * TILE, r0 * TILE);
                done.push(Rect {
                    x,
                    y,
                    w: (c1 * TILE).min(width) - x,
                    h: (r * TILE).min(height) - y,
                });
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

/// Average `f`x`f` blocks of a region into a smaller BGRA buffer. Blocks cut
/// off by the region's edge average only the pixels they have.
fn downscale(screen: &Screen, r: Rect, f: usize) -> (Vec<u8>, usize, usize) {
    let stride = screen.width * 4;
    let (ow, oh) = (r.w.div_ceil(f), r.h.div_ceil(f));
    let mut out = vec![255u8; ow * oh * 4];
    let mut sums = vec![0u32; ow * 3];
    for oy in 0..oh {
        sums.fill(0);
        let y0 = r.y + oy * f;
        let rows = f.min(r.y + r.h - y0);
        for y in y0..y0 + rows {
            let line = &screen.buf[y * stride + r.x * 4..][..r.w * 4];
            for (block, sum) in line.chunks(f * 4).zip(sums.chunks_exact_mut(3)) {
                for px in block.chunks_exact(4) {
                    sum[0] += px[0] as u32;
                    sum[1] += px[1] as u32;
                    sum[2] += px[2] as u32;
                }
            }
        }
        let dst = &mut out[oy * ow * 4..][..ow * 4];
        for (ox, (px, sum)) in dst.chunks_exact_mut(4).zip(sums.chunks_exact(3)).enumerate() {
            let n = (f.min(r.w - ox * f) * rows) as u32;
            for c in 0..3 {
                px[c] = ((sum[c] + n / 2) / n) as u8;
            }
        }
    }
    (out, ow, oh)
}

/// A region shrunk by `factor`, as packed BGRA rows: borrowed from the
/// screen when it already is (full-width rows at full size), else copied.
fn region(screen: &Screen, r: Rect, factor: usize) -> (Cow<'_, [u8]>, usize, usize) {
    let stride = screen.width * 4;
    if factor > 1 {
        let (buf, ow, oh) = downscale(screen, r, factor);
        (Cow::Owned(buf), ow, oh)
    } else if r.x == 0 && r.w == screen.width {
        (Cow::Borrowed(&screen.buf[r.y * stride..(r.y + r.h) * stride]), r.w, r.h)
    } else {
        let mut buf = Vec::with_capacity(r.w * r.h * 4);
        for y in r.y..r.y + r.h {
            buf.extend_from_slice(&screen.buf[y * stride + r.x * 4..][..r.w * 4]);
        }
        (Cow::Owned(buf), r.w, r.h)
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
    let (pixels, ow, oh) = region(screen, r, factor);
    let mut out = Vec::with_capacity(ow * oh / 3 + 1024);
    let mut encoder = Encoder::new(&mut out, quality);
    encoder.set_sampling_factor(if full_chroma {
        SamplingFactor::R_4_4_4
    } else {
        SamplingFactor::R_4_2_0
    });
    encoder
        .encode(&pixels, ow as u16, oh as u16, ColorType::Bgra)
        .expect("JPEG encoding into memory cannot fail");
    (on_canvas(r, scale), out)
}

/// Colour of tiles painted over in a lossless piece (they go out as JPEG).
const FLAT: [u8; 3] = [128, 128, 128];

/// Losslessly encode one region at the canvas scale as WebP. Tiles not in
/// `exact` are painted flat: they are sent as JPEG and drawn over it. At full
/// size the pixels are read from the screen in place, with no copy; at half
/// size from the shrunk copy, a quarter of the region.
fn encode_lossless(screen: &Screen, r: Rect, scale: usize, exact: &[bool]) -> (Rect, Vec<u8>) {
    let (pixels, stride) = if scale > 1 {
        let (buf, ow, _) = downscale(screen, r, scale);
        (Cow::Owned(buf), ow * 4)
    } else {
        (Cow::Borrowed(&screen.buf[(r.y * screen.width + r.x) * 4..]), screen.width * 4)
    };
    let (ow, oh) = (r.w.div_ceil(scale), r.h.div_ceil(scale));
    let out = webp::encode(ow, oh, |oy, row| {
        let line = &pixels[oy * stride..][..ow * 4];
        let tiles = &exact[(r.y + oy * scale) / TILE * screen.cols..];
        for (ox, (dst, px)) in row.iter_mut().zip(line.chunks_exact(4)).enumerate() {
            // BGRA in, with an undefined 4th byte; RGB out.
            *dst = if tiles[(r.x + ox * scale) / TILE] { [px[2], px[1], px[0]] } else { FLAT };
        }
    });
    (on_canvas(r, scale), out)
}

/// Whether a tile looks like text or flat UI, which lossless compression
/// packs smaller than JPEG: at least three quarters of its pixels repeat the
/// one to their left or above. Photos, and gradients with dithering noise,
/// have few repeats however few colours they use.
fn looks_flat(screen: &Screen, tile: usize) -> bool {
    let (x0, y0) = (tile % screen.cols * TILE, tile / screen.cols * TILE);
    let (w, h) = (TILE.min(screen.width - x0), TILE.min(screen.height - y0));
    let stride = screen.width * 4;
    // The 4th byte of a captured pixel is undefined.
    let px = |x: usize, y: usize| u32::from_le_bytes(screen.buf[y * stride + x * 4..][..4].try_into().unwrap()) & 0xFF_FFFF;
    let mut repeats = 0;
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let v = px(x, y);
            if (x > x0 && v == px(x - 1, y)) || (y > y0 && v == px(x, y - 1)) {
                repeats += 1;
            }
        }
    }
    repeats * 4 >= w * h * 3
}

/// Photo tiles widened, within each row's runs of covered tiles, to span from
/// the first photo tile to the last: fewer, larger JPEGs, never over a tile
/// the frame does not send.
fn photo_spans(covered: &[bool], photo: &[bool], cols: usize) -> Vec<bool> {
    let mut spans = vec![false; covered.len()];
    for row in (0..covered.len()).step_by(cols) {
        let mut c = 0;
        while c < cols {
            if !covered[row + c] {
                c += 1;
                continue;
            }
            let start = c;
            while c < cols && covered[row + c] {
                c += 1;
            }
            let run = row + start..row + c;
            if let (Some(a), Some(b)) = (run.clone().position(|t| photo[t]), run.clone().rposition(|t| photo[t])) {
                spans[run.start + a..=run.start + b].fill(true);
            }
        }
    }
    spans
}

/// Mark the tiles a list of rectangles covers.
fn cover(rects: &[Rect], cols: usize, tiles: &mut [bool]) {
    for r in rects {
        for ty in r.y / TILE..(r.y + r.h).div_ceil(TILE) {
            tiles[ty * cols + r.x / TILE..ty * cols + (r.x + r.w).div_ceil(TILE)].fill(true);
        }
    }
}

/// Cut rectangles into pieces at most CHUNK_ROWS high, to encode in parallel.
fn chunks(rects: Vec<Rect>) -> Vec<Rect> {
    let mut pieces = Vec::new();
    for r in rects {
        let mut y = r.y;
        while y < r.y + r.h {
            let h = CHUNK_ROWS.min(r.y + r.h - y);
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
    let rects = merge(tiles, screen.cols, screen.rows, screen.width, screen.height);
    let mut covered = vec![false; tiles.len()];
    cover(&rects, screen.cols, &mut covered);
    let (lossless, jpeg, exact) = if flags & FLAG_LOSSLESS != 0 {
        let mut exact: Vec<bool> = (0..tiles.len()).into_par_iter().map(|t| covered[t] && looks_flat(screen, t)).collect();
        let photo: Vec<bool> = covered.iter().zip(&exact).map(|(&c, &e)| c && !e).collect();
        // Many small JPEGs would cost more in headers and seams than drawing
        // over some flat tiles, which are then not exact after all.
        let spans = photo_spans(&covered, &photo, screen.cols);
        let jpeg = stack(&spans, screen.cols, screen.rows, screen.width, screen.height, false);
        let mut drawn_over = vec![false; tiles.len()];
        cover(&jpeg, screen.cols, &mut drawn_over);
        for (e, &d) in exact.iter_mut().zip(&drawn_over) {
            *e &= !d;
        }
        let has_exact = |r: &Rect| {
            (r.y / TILE..(r.y + r.h).div_ceil(TILE))
                .any(|ty| exact[ty * screen.cols + r.x / TILE..ty * screen.cols + (r.x + r.w).div_ceil(TILE)].contains(&true))
        };
        let lossless: Vec<Rect> = chunks(rects).into_iter().filter(has_exact).collect();
        (lossless, chunks(jpeg), exact)
    } else {
        (Vec::new(), chunks(rects), Vec::new())
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
    cap: Source,
    screens: HashMap<usize, Screen>,
    sessions: Vec<Session>,
    shared: Arc<Shared>,
    interval: Duration,
    last_capture: Instant,
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
        cap: Source::new(),
        screens: HashMap::new(),
        sessions: Vec::new(),
        shared,
        interval: Duration::from_secs_f64(1.0 / fps as f64),
        last_capture: Instant::now(),
        retry_at: None,
        failing: false,
        stats: Stats::default(),
    };
    e.publish_monitors();
    loop {
        if !e.sessions.iter().any(|s| s.visible) {
            // Nobody is watching: no capture at all.
            let cmd = match rx.recv_timeout(IDLE_RELEASE) {
                Ok(cmd) => cmd,
                Err(RecvTimeoutError::Timeout) => {
                    e.cap.release();
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
        while let Ok(cmd) = rx.try_recv() {
            e.handle(cmd);
        }
        // Wait out a capture failure, or the rest of this frame's time slot,
        // while still serving commands.
        let now = Instant::now();
        let ready_at = e.retry_at.unwrap_or(e.last_capture + e.capture_interval());
        if now < ready_at {
            match rx.recv_timeout(ready_at - now) {
                Ok(cmd) => e.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            continue;
        }
        e.retry_at = None;
        e.capture();
        e.flush_all();
    }
}

impl Engine {
    /// Time between captures: the host's limit, or slower if every client
    /// watching asked for fewer frames. Changes accumulate in between, so a
    /// slower rate sends less without losing anything.
    fn capture_interval(&self) -> Duration {
        capture_interval(self.interval, self.sessions.iter().filter(|s| s.visible).map(|s| s.view.fps))
    }

    fn publish_monitors(&mut self) {
        let monitors = self.cap.monitors();
        let mut shared = self.shared.monitors.write().unwrap();
        if *shared != monitors {
            crate::log!(
                "monitors: {}",
                monitors
                    .iter()
                    .map(|m| format!("{}x{}@{},{}", m.width, m.height, m.x, m.y))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            *shared = monitors;
            drop(shared);
            for i in 0..self.sessions.len() {
                self.hello(i);
            }
        }
    }

    fn hello(&mut self, index: usize) {
        let monitors = self.shared.monitors.read().unwrap().clone();
        let s = &mut self.sessions[index];
        s.view.mon = s.view.mon.min(monitors.len().saturating_sub(1));
        s.dims = (0, 0);
        let list: Vec<_> = monitors
            .iter()
            .map(|m| serde_json::json!({ "w": m.width, "h": m.height }))
            .collect();
        s.send_text(
            serde_json::json!({
                "t": "hello",
                "monitors": list,
                "mon": s.view.mon,
                "viewOnly": self.shared.view_only,
                "clipboard": self.shared.clipboard,
            })
            .to_string(),
        );
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Add { id, tx, view, visible } => {
                let now = Instant::now();
                self.sessions.push(Session {
                    id,
                    tx,
                    view,
                    visible,
                    dims: (0, 0),
                    tiles: Tiles::default(),
                    pending_since: None,
                    seq: 0,
                    acked: 0,
                    last_send: now,
                    cursor: "",
                    blocked: false,
                });
                self.hello(self.sessions.len() - 1);
            }
            Cmd::Remove { id } => self.sessions.retain(|s| s.id != id),
            Cmd::Ack { id, seq } => {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
                    let s = &mut self.sessions[i];
                    // Acks can only move forward (seq wraps after 4 billion frames).
                    if (seq.wrapping_sub(s.acked) as i32) > 0 && (s.seq.wrapping_sub(seq) as i32) >= 0 {
                        s.acked = seq;
                    }
                    self.flush(i, Instant::now());
                }
            }
            Cmd::Visible { id, visible } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.visible = visible;
                    // Changes from while it was asleep are not latency.
                    s.pending_since = None;
                }
            }
            Cmd::Configure { id, change } => {
                if let Some(i) = self.sessions.iter().position(|s| s.id == id) {
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
                        self.hello(i); // clamps the monitor index and forces a full resend
                    }
                }
            }
        }
    }

    /// Capture every monitor a visible session is looking at.
    fn capture(&mut self) {
        let mut wanted: Vec<usize> =
            self.sessions.iter().filter(|s| s.visible).map(|s| s.view.mon).collect();
        wanted.sort_unstable();
        wanted.dedup();
        // If a client is already owed something it has room for, don't sit
        // waiting for the screen to change before sending it. (One waiting
        // on acks is not urgent: polling for it would spin the CPU.)
        let urgent = self.sessions.iter().any(|s| {
            s.visible
                && (s.dims == (0, 0)
                    || (s.seq.wrapping_sub(s.acked) < MAX_INFLIGHT && s.tiles.dirty.iter().any(|&d| d)))
        });

        for (n, &mon) in wanted.iter().enumerate() {
            let timeout = if urgent || n > 0 { Duration::ZERO } else { ACQUIRE_WAIT };
            let screen = self.screens.entry(mon).or_default();
            match self.cap.acquire(mon, timeout, |px| screen.update(px)) {
                Ok(true) => {
                    let now = Instant::now();
                    self.last_capture = now;
                    self.stats.capture += self.cap.last_cost;
                    // The image is as of just before it was copied off the GPU.
                    let captured = now.checked_sub(self.cap.last_cost).unwrap_or(now);
                    if self.failing {
                        self.failing = false;
                        crate::log!("capture recovered");
                    }
                    let changed = screen.changed.iter().any(|&c| c);
                    for s in self.sessions.iter_mut().filter(|s| s.view.mon == mon) {
                        if s.dims == (screen.width, screen.height) {
                            for (d, c) in s.tiles.dirty.iter_mut().zip(&screen.changed) {
                                *d |= *c;
                            }
                        }
                        if changed && s.visible {
                            s.pending_since.get_or_insert(captured);
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
                    self.publish_monitors();
                    self.retry_at = Some(Instant::now() + RETRY);
                    return;
                }
            }
        }
    }

    fn flush_all(&mut self) {
        let cursor = platform::cursor_name();
        let blocked = platform::input_blocked();
        let now = Instant::now();
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
            self.flush(i, now);
        }
        self.report(now);
    }

    /// Send session `index` whatever it is owed, if it has room for a frame.
    fn flush(&mut self, index: usize, now: Instant) {
        let s = &mut self.sessions[index];
        let Some(screen) = self.screens.get(&s.view.mon) else { return };
        if !s.visible || screen.width == 0 {
            return;
        }
        // A whole new picture (just connected, or switched monitor, quality or
        // resolution) goes out sharp straight away rather than blurry and
        // then again sharp.
        // (Checked before the room for a frame, so that a fresh picture still
        // counts as fresh once there is room.)
        if s.seq.wrapping_sub(s.acked) >= MAX_INFLIGHT {
            if now.duration_since(s.last_send) < STALL {
                return;
            }
            s.acked = s.seq;
        }
        let fresh = s.dims != (screen.width, screen.height);
        if fresh {
            s.dims = (screen.width, screen.height);
            s.tiles.reset(screen.cols * screen.rows, now);
        }

        let Some((tiles, pass)) = s.tiles.next(now, fresh) else { return };
        let sharp = || {
            let (quality, full_chroma) = s.view.quality.still();
            (quality, FLAG_REFINE | if full_chroma { FLAG_FULL_CHROMA } else { 0 })
        };
        let ((quality, mut flags), since) = match pass {
            Pass::Fresh => (sharp(), s.pending_since.take()),
            Pass::Refine => (sharp(), None),
            Pass::Moving => {
                let flags = if s.view.soft { FLAG_SOFT } else { 0 };
                ((s.view.quality.moving(), flags), s.pending_since.take())
            }
        };
        // Soft motion is the cheaper way to send what keeps changing; the
        // lossless re-send follows once it stops.
        if s.view.lossless && flags & FLAG_SOFT == 0 {
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
        s.last_send = now;
        (s.tx)(Out::Binary(msg, crate::screen::now_us()));
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
        s.update(Pixels { width, height, stride: width * 4, data: &data });
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
        s.update(Pixels { width: 200, height: 100, stride, data: &data });
        let flagged: Vec<usize> = (0..8).filter(|&i| s.changed[i]).collect();
        assert_eq!(flagged, vec![1, 7]); // tile (1,0) and the partial tile (3,1)
        s.update(Pixels { width: 200, height: 100, stride, data: &data });
        assert!(s.changed.iter().all(|&c| !c), "identical frame changes nothing");
        assert_eq!(s.buf[10 * 800 + 70 * 4], 200);
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
        s.update(Pixels { width: 200, height: 150, stride: 800, data: &data });
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
        let mut cap = platform::Capture::new();
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

        let (sent, pass) = tiles.next(at(0), true).unwrap();
        assert_eq!((on(&sent), pass), (vec![0, 1, 2, 3], Pass::Fresh));
        assert!(tiles.next(at(1), false).is_none(), "a fresh picture is already sharp");

        // A scroll changes everything, then a spinner keeps animating tile 3.
        tiles.dirty = vec![true; 4];
        let (sent, pass) = tiles.next(at(10), false).unwrap();
        assert_eq!((on(&sent), pass), (vec![0, 1, 2, 3], Pass::Moving));
        for ms in (30..250).step_by(20) {
            tiles.dirty[3] = true;
            let (sent, pass) = tiles.next(at(ms), false).unwrap();
            assert_eq!((on(&sent), pass), (vec![3], Pass::Moving), "at {ms} ms");
        }
        // 250 ms after the scroll, tiles 0-2 are sharpened even though 3 still moves...
        tiles.dirty[3] = true;
        let (sent, pass) = tiles.next(at(260), false).unwrap();
        assert_eq!((on(&sent), pass), (vec![0, 1, 2], Pass::Refine));
        // ...and the spinner's update goes out right after.
        let (sent, pass) = tiles.next(at(261), false).unwrap();
        assert_eq!((on(&sent), pass), (vec![3], Pass::Moving));

        // Once the spinner stops, its tile is sharpened too.
        assert!(tiles.next(at(400), false).is_none());
        let (sent, pass) = tiles.next(at(520), false).unwrap();
        assert_eq!((on(&sent), pass), (vec![3], Pass::Refine));
        assert!(tiles.next(at(900), false).is_none());
    }

    #[test]
    fn tiles_a_whole_row_merge_draws_blurry_get_sharpened_too() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let s = screen(1920, 256);
        let mut tiles = Tiles::default();
        tiles.reset(s.cols * s.rows, t0);
        tiles.next(at(0), true);
        // A checkerboard of changed tiles: more runs than MAX_RECTS, so the
        // frame draws whole rows, from the first changed tile to the last.
        let (col, row) = (|t: usize| t % s.cols, |t: usize| t / s.cols);
        tiles.dirty = (0..s.cols * s.rows).map(|t| (col(t) + row(t)) % 2 == 0).collect();
        let (sent, pass) = tiles.next(at(10), false).unwrap();
        assert_eq!(pass, Pass::Moving);
        let frame = frame_message(&s, &sent, 1, 70, 0, 1);
        let drawn: Vec<bool> = (0..s.cols * s.rows).map(|t| if row(t) % 2 == 0 { col(t) < 29 } else { col(t) > 0 }).collect();
        assert_eq!(frame.drawn, drawn);
        tiles.sent(&frame.drawn, &frame.exact, true, at(10));
        // Once still, everything drawn blurry is re-sent sharp.
        assert_eq!(tiles.next(at(300), false), Some((drawn, Pass::Refine)));
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
        s.update(Pixels { width: w, height: h, stride: w * 4, data: &data });
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
        s.update(Pixels { width, height, stride: width * 4, data: &data });
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
        // Photos in tiles 0 and 2 of a row of four, text in 1 and 3.
        let s = painted(256, 64, |x, y| if x / 64 % 2 == 0 { noise(x, y) } else { [255, 255, (x / 3 % 2 * 255) as u8] });
        let jpegs = |tiles: &[bool]| {
            let Frame { msg, exact, .. } = frame_message(&s, tiles, 1, 70, FLAG_LOSSLESS, 6);
            let rects = parts(&msg).iter().filter(|p| p.4 == 0).map(|p| (p.0, p.2)).collect::<Vec<_>>();
            (rects, exact)
        };
        // All sent: one JPEG over tiles 0 to 2, so tile 1 is not exact.
        assert_eq!(jpegs(&[true; 4]), (vec![(0, 192)], vec![false, false, false, true]));
        // Tile 1 not sent: it must not be drawn over.
        assert_eq!(jpegs(&[true, false, true, false]), (vec![(0, 64), (128, 64)], vec![false, false, false, false]));
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
