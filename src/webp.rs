//! Lossless WebP (VP8L) encoding of opaque images, one row at a time.
//!
//! Uses subtract-green and "pixel above" prediction, runs of the previous
//! residual, and a color cache when it reduces the encoded size. The Huffman
//! builder is adapted from image-webp 0.2.4 by the image-rs developers
//! (https://github.com/image-rs/image-webp, MIT or Apache-2.0). The encoder
//! asks for each row twice and keeps only the current and previous row.

use std::collections::BinaryHeap;

const RUN_START: usize = 256;
const CACHE_START: usize = RUN_START + 24;
const CACHE_BITS: u8 = 6;
const CACHE_SIZE: usize = 1 << CACHE_BITS;
const GREEN_SIZE: usize = CACHE_START + CACHE_SIZE;

struct ColorCache {
    entries: [u32; CACHE_SIZE],
}

impl ColorCache {
    #[inline]
    fn lookup_or_insert(&mut self, argb: u32) -> Option<usize> {
        let index = (0x1e35a7bd_u32.wrapping_mul(argb) >> (32 - CACHE_BITS)) as usize;
        let previous = std::mem::replace(&mut self.entries[index], argb);
        (previous == argb).then_some(index)
    }
}

struct Frequencies {
    green: [u32; GREEN_SIZE],
    red: [u32; 256],
    blue: [u32; 256],
}

impl Frequencies {
    fn new() -> Self {
        Self { green: [0; GREEN_SIZE], red: [0; 256], blue: [0; 256] }
    }

    #[inline]
    fn count_pixel(&mut self, argb: u32) {
        let [_, r, g, b] = argb.to_be_bytes();
        self.green[g as usize] += 1;
        self.red[r as usize] += 1;
        self.blue[b as usize] += 1;
    }

    fn add_cache_hits_as_literals(&mut self, hits: &Self) {
        for i in 0..256 {
            self.green[i] += hits.green[i];
            self.red[i] += hits.red[i];
            self.blue[i] += hits.blue[i];
        }
    }
}

/// Encode a `width` x `height` opaque image. `row(y, out)` fills `out` with
/// row `y` as RGB; it is called for every row, top to bottom, twice.
pub fn encode(width: usize, height: usize, mut row: impl FnMut(usize, &mut [[u8; 3]])) -> Vec<u8> {
    assert!((1..=16384).contains(&width) && (1..=16384).contains(&height), "WebP sizes are 1 to 16384");

    // Count both variants in one pass. A run repeats the last residual, which
    // is already in the cache, so it needs no extra lookups or insertions.
    let mut frequencies = Frequencies::new();
    let mut hits = Frequencies::new();
    let mut cache = ColorCache { entries: [0; CACHE_SIZE] };
    let mut has_hits = false;
    let mut run_extra_bits = 0;
    residuals(width, height, &mut row, |symbol| match symbol {
        Symbol::Pixel(argb) => {
            if let Some(index) = cache.lookup_or_insert(argb) {
                frequencies.green[CACHE_START + index] += 1;
                hits.count_pixel(argb);
                has_hits = true;
            } else {
                frequencies.count_pixel(argb);
            }
        }
        Symbol::Run(n) => {
            let (symbol, extra) = run_symbol(n);
            frequencies.green[RUN_START + symbol as usize] += 1;
            run_extra_bits += usize::from(extra);
        }
    });

    // RIFF and VP8L chunk headers, sizes filled in at the end.
    let mut w = BitWriter { out: Vec::with_capacity(128), buffer: 0, nbits: 0 };
    w.out.extend_from_slice(b"RIFF\0\0\0\0WEBPVP8L\0\0\0\0");
    w.write_bits(0x2f, 8); // signature
    w.write_bits(width as u64 - 1, 14);
    w.write_bits(height as u64 - 1, 14);
    w.write_bits(0, 1); // alpha not used
    w.write_bits(0, 3); // version
    w.write_bits(0b101, 3); // subtract-green transform
    // Predictor transform in 512-pixel blocks, every block "pixel above".
    w.write_bits(0b111001, 6);
    w.write_bits(0, 1); // no colour cache
    write_simple_huffman_tree(&mut w, 2, None);
    for _ in 0..4 {
        write_simple_huffman_tree(&mut w, 0, None);
    }
    w.write_bits(0, 1); // no more transforms

    let cached = has_hits.then(|| {
        let cached = encode_header(w.clone(), &frequencies, true, run_extra_bits);
        frequencies.add_cache_hits_as_literals(&hits);
        cached
    });
    let plain = encode_header(w, &frequencies, false, run_extra_bits);
    let (mut w, codes, size) = cached.filter(|cached| cached.2 < plain.2).unwrap_or(plain);
    w.out.reserve(size - w.out.len());

    // Pass 2: the pixels.
    if codes.use_cache {
        write_pixels::<true>(&mut w, &codes, width, height, &mut row);
    } else {
        write_pixels::<false>(&mut w, &codes, width, height, &mut row);
    }
    w.flush();

    let mut out = w.out;
    let frame = out.len() - 20;
    if frame % 2 == 1 {
        out.push(0);
    }
    debug_assert_eq!(out.len(), size);
    let riff = out.len() as u32 - 8;
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    out[16..20].copy_from_slice(&(frame as u32).to_le_bytes());
    out
}

struct ImageCodes {
    green: [(u16, u8); GREEN_SIZE],
    red: [(u16, u8); 256],
    blue: [(u16, u8); 256],
    use_cache: bool,
}

fn encode_header(
    mut w: BitWriter,
    frequencies: &Frequencies,
    use_cache: bool,
    run_extra_bits: usize,
) -> (BitWriter, ImageCodes, usize) {
    w.write_bits(u64::from(use_cache), 1);
    if use_cache {
        w.write_bits(u64::from(CACHE_BITS), 4);
    }
    w.write_bits(0, 1); // no meta Huffman codes

    let mut codes = ImageCodes {
        green: [(0, 0); GREEN_SIZE], red: [(0, 0); 256], blue: [(0, 0); 256], use_cache,
    };
    let green_size = if use_cache { GREEN_SIZE } else { CACHE_START };
    let green_bits = write_channel(&mut w, &frequencies.green[..green_size], &mut codes.green[..green_size]);
    let red_bits = write_channel(&mut w, &frequencies.red, &mut codes.red);
    let blue_bits = write_channel(&mut w, &frequencies.blue, &mut codes.blue);
    write_simple_huffman_tree(&mut w, 0, None); // alpha: always 0 after prediction
    write_simple_huffman_tree(&mut w, 1, None); // distance: the previous pixel

    let bits = w.out.len() as u64 * 8 + u64::from(w.nbits) + green_bits + red_bits + blue_bits + run_extra_bits as u64;
    let size = bits.div_ceil(8).next_multiple_of(2) as usize; // RIFF padding
    (w, codes, size)
}

fn write_channel(w: &mut BitWriter, frequencies: &[u32], table: &mut [(u16, u8)]) -> u64 {
    let mut lengths = [0; GREEN_SIZE];
    let mut codes = [0; GREEN_SIZE];
    let len = frequencies.len();
    write_huffman_tree(w, frequencies, &mut lengths[..len], &mut codes[..len]);
    let mut bits = 0;
    for (i, (&frequency, entry)) in frequencies.iter().zip(table).enumerate() {
        *entry = (codes[i], lengths[i]);
        bits += u64::from(frequency) * u64::from(lengths[i]);
    }
    bits
}

fn write_pixels<const CACHE: bool>(
    w: &mut BitWriter,
    codes: &ImageCodes,
    width: usize,
    height: usize,
    row: &mut impl FnMut(usize, &mut [[u8; 3]]),
) {
    let mut cache = ColorCache { entries: [0; CACHE_SIZE] };
    residuals(width, height, row, |symbol| match symbol {
        Symbol::Pixel(argb) => {
            if CACHE && let Some(index) = cache.lookup_or_insert(argb) {
                let (code, len) = codes.green[CACHE_START + index];
                w.write_bits(u64::from(code), len);
                return;
            }
            let [_, r, g, b] = argb.to_be_bytes();
            let (green, lg) = codes.green[g as usize];
            let (red, lr) = codes.red[r as usize];
            let (blue, lb) = codes.blue[b as usize];
            let code = u64::from(green) | u64::from(red) << lg | u64::from(blue) << (lg + lr);
            w.write_bits(code, lg + lr + lb);
        }
        Symbol::Run(n) => {
            let (symbol, extra) = run_symbol(n);
            let (code, len) = codes.green[RUN_START + symbol as usize];
            let bits = (n as u64 - 1) & ((1 << extra) - 1);
            w.write_bits(u64::from(code) | bits << len, len + extra);
        }
    });
}

enum Symbol {
    /// An ARGB pixel after both transforms. The alpha residual is always zero.
    Pixel(u32),
    /// That many more copies of the previous pixel (1 to 4096).
    Run(usize),
}

/// The pixels after the transforms, in order, with repeats folded into runs.
fn residuals(width: usize, height: usize, row: &mut impl FnMut(usize, &mut [[u8; 3]]), mut emit: impl FnMut(Symbol)) {
    let mut prev = vec![[0u8; 3]; width];
    let mut cur = vec![[0u8; 3]; width];
    let mut last = u32::MAX; // outside the range of ARGB residuals
    let mut run = 0;
    for y in 0..height {
        row(y, &mut cur);
        // Predict from the pixel above. The first row instead uses the
        // pixel to the left, with black at the top-left corner.
        let mut predict = |(pixel, predicted): (&[u8; 3], &[u8; 3])| {
            let res = if pixel == predicted {
                0
            } else {
                let ([r, g, b], [pr, pg, pb]) = (*pixel, *predicted);
                // Subtract-green and prediction commute under wrapping arithmetic.
                let g = g.wrapping_sub(pg);
                u32::from_be_bytes([0, r.wrapping_sub(pr).wrapping_sub(g), g, b.wrapping_sub(pb).wrapping_sub(g)])
            };
            if last == res && run < 4096 {
                run += 1;
                return;
            }
            if run > 0 {
                emit(Symbol::Run(run));
                run = 0;
            }
            emit(Symbol::Pixel(res));
            last = res;
        };
        if y == 0 {
            cur.iter().zip(std::iter::once(&[0; 3]).chain(&cur)).for_each(predict);
        } else {
            cur.iter().zip(&prev).for_each(&mut predict);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    if run > 0 {
        emit(Symbol::Run(run));
    }
}

/// The length prefix symbol and extra bits for a run of `n` (LZ77 length).
fn run_symbol(n: usize) -> (u16, u8) {
    if n <= 4 {
        return (n as u16 - 1, 0);
    }
    let len = n as u16 - 1;
    let highest_bit = len.ilog2() as u16;
    let second_highest_bit = (len >> (highest_bit - 1)) & 1;
    (2 * highest_bit + second_highest_bit, (highest_bit - 1) as u8)
}

#[derive(Clone)]
struct BitWriter {
    out: Vec<u8>,
    buffer: u64,
    nbits: u8,
}

impl BitWriter {
    fn write_bits(&mut self, bits: u64, nbits: u8) {
        debug_assert!(nbits <= 64);
        self.buffer |= bits << self.nbits;
        self.nbits += nbits;
        if self.nbits >= 64 {
            self.out.extend_from_slice(&self.buffer.to_le_bytes());
            self.nbits -= 64;
            self.buffer = bits.checked_shr(u32::from(nbits - self.nbits)).unwrap_or(0);
        }
    }

    fn flush(&mut self) {
        if self.nbits % 8 != 0 {
            self.write_bits(0, 8 - self.nbits % 8);
        }
        if self.nbits > 0 {
            self.out.extend_from_slice(&self.buffer.to_le_bytes()[..self.nbits as usize / 8]);
            self.buffer = 0;
            self.nbits = 0;
        }
    }
}

fn write_simple_huffman_tree(w: &mut BitWriter, symbol: u8, second: Option<u8>) {
    w.write_bits(1 | (u64::from(second.is_some()) << 1), 2);
    if symbol <= 1 {
        w.write_bits(0, 1);
        w.write_bits(u64::from(symbol), 1);
    } else {
        w.write_bits(1, 1);
        w.write_bits(u64::from(symbol), 8);
    }
    if let Some(second) = second {
        w.write_bits(u64::from(second), 8);
    }
}

/// Canonical Huffman code lengths and (bit-reversed) codes, at most
/// `length_limit` bits. False, with everything zero, for one symbol or none.
fn build_huffman_tree(frequencies: &[u32], lengths: &mut [u8], codes: &mut [u16], length_limit: u8) -> bool {
    assert_eq!(frequencies.len(), lengths.len());
    assert_eq!(frequencies.len(), codes.len());

    let num_leaves = frequencies.iter().filter(|&&f| f > 0).count();
    if num_leaves <= 1 {
        lengths.fill(0);
        codes.fill(0);
        return false;
    }

    #[derive(Eq, PartialEq, Copy, Clone, Debug)]
    struct Item(u32, u16);
    impl Ord for Item {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other.0.cmp(&self.0)
        }
    }
    impl PartialOrd for Item {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    // Build a Huffman tree.
    let mut leaves = Vec::with_capacity(num_leaves);
    leaves.extend(frequencies.iter().enumerate().filter(|(_, f)| **f > 0).map(|(i, &f)| Item(f, i as u16)));
    let mut nodes = BinaryHeap::from(leaves);
    let mut internal_nodes = Vec::with_capacity(num_leaves - 1);
    while nodes.len() > 1 {
        let Item(frequency1, index1) = nodes.pop().unwrap();
        let mut root = nodes.peek_mut().unwrap();
        internal_nodes.push((index1, root.1, 0));
        *root = Item(frequency1 + root.0, internal_nodes.len() as u16 + frequencies.len() as u16 - 1);
    }

    // Parents are created after their children, so walk backward from the root.
    lengths.fill(0);
    for i in (0..internal_nodes.len()).rev() {
        let (left, right, depth) = internal_nodes[i];
        for child in [left, right] {
            let child = child as usize;
            if child < frequencies.len() {
                lengths[child] = depth + 1;
            } else {
                internal_nodes[child - frequencies.len()].2 = depth + 1;
            }
        }
    }

    // Limit the codes to `length_limit` bits.
    let max_length = lengths.iter().copied().max().unwrap_or(0);
    if max_length > length_limit {
        let mut counts = [0u32; 16];
        for &length in lengths.iter() {
            counts[length.min(length_limit) as usize] += 1;
        }
        let mut total = 0;
        for (i, count) in counts.iter().enumerate().skip(1).take(length_limit as usize) {
            total += count << (length_limit as usize - i);
        }
        while total > 1u32 << length_limit {
            let mut i = length_limit as usize - 1;
            while counts[i] == 0 {
                i -= 1;
            }
            counts[i] -= 1;
            counts[length_limit as usize] -= 1;
            counts[i + 1] += 2;
            total -= 1;
        }
        let mut len = length_limit;
        let mut indexes = frequencies.iter().copied().enumerate().collect::<Vec<_>>();
        indexes.sort_unstable_by_key(|&(_, frequency)| frequency);
        for &(i, frequency) in &indexes {
            if frequency > 0 {
                while counts[len as usize] == 0 {
                    len -= 1;
                }
                lengths[i] = len;
                counts[len as usize] -= 1;
            }
        }
    }

    // Assign canonical codes in one pass over the alphabet.
    let mut counts = [0u16; 16];
    for &length in lengths.iter() {
        counts[length as usize] += 1;
    }
    let mut next_codes = [0u16; 16];
    for len in 2..=length_limit as usize {
        next_codes[len] = (next_codes[len - 1] + counts[len - 1]) << 1;
    }
    let limit = length_limit as usize;
    assert_eq!(u32::from(next_codes[limit]) + u32::from(counts[limit]), 1 << limit);
    for (&length, code) in lengths.iter().zip(codes) {
        *code = 0;
        if length != 0 {
            let next = &mut next_codes[length as usize];
            *code = next.reverse_bits() >> (16 - length);
            *next += 1;
        }
    }
    true
}

fn write_huffman_tree(w: &mut BitWriter, frequencies: &[u32], lengths: &mut [u8], codes: &mut [u16]) {
    let mut symbols = frequencies.iter().enumerate().filter(|&(_, &count)| count != 0).map(|(i, _)| i);
    let first = symbols.next().unwrap_or(0);
    let second = symbols.next();
    if first < 256 && second.is_none_or(|i| i < 256) && symbols.next().is_none() {
        if let Some(second) = second {
            lengths[first] = 1;
            lengths[second] = 1;
            codes[second] = 1;
        }
        write_simple_huffman_tree(w, first as u8, second.map(|i| i as u8));
        return;
    }

    let single_symbol = !build_huffman_tree(frequencies, lengths, codes, 15);
    if single_symbol {
        // The simple format cannot represent a run or color cache symbol.
        lengths[first] = 1;
    }

    const CODE_LENGTH_ORDER: [usize; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    let used_lengths = lengths.iter().rposition(|&len| len != 0).unwrap() + 1;
    let mut tokens = [LengthToken { symbol: 0, extra: 0, extra_bits: 0 }; GREEN_SIZE];
    let tokens = encode_lengths(&lengths[..used_lengths], &mut tokens);
    let mut frequencies = [0u32; 19];
    for token in tokens {
        frequencies[token.symbol as usize] += 1;
    }

    let mut token_lengths = [0u8; 19];
    let mut token_codes = [0u16; 19];
    let single_token_symbol = !build_huffman_tree(&frequencies, &mut token_lengths, &mut token_codes, 7);
    if single_token_symbol {
        token_lengths[tokens[0].symbol as usize] = 1;
    }
    let num_code_lengths = CODE_LENGTH_ORDER.iter().rposition(|&i| token_lengths[i] != 0).unwrap() + 1;

    w.write_bits(0, 1); // normal Huffman tree
    let num_code_lengths = num_code_lengths.max(4);
    w.write_bits((num_code_lengths - 4) as u64, 4);
    for &i in &CODE_LENGTH_ORDER[..num_code_lengths] {
        w.write_bits(u64::from(token_lengths[i]), 3);
    }
    if used_lengths < lengths.len() {
        // The limit counts RLE tokens, not the individual lengths they expand to.
        let max_symbol = tokens.len().max(2);
        let extra = max_symbol - 2;
        let nbit_pairs = (extra.checked_ilog2().unwrap_or(0) / 2 + 1) as u8;
        w.write_bits(1, 1);
        w.write_bits(u64::from(nbit_pairs - 1), 3);
        w.write_bits(extra as u64, nbit_pairs * 2);
    } else {
        w.write_bits(0, 1); // the tokens will fill the alphabet
    }
    for token in tokens {
        let i = token.symbol as usize;
        let len = if single_token_symbol { 0 } else { token_lengths[i] };
        w.write_bits(u64::from(token_codes[i]) | u64::from(token.extra) << len, len + token.extra_bits);
    }
    if single_symbol {
        lengths[first] = 0; // a one-leaf tree consumes no bits
    }
}

#[derive(Clone, Copy)]
struct LengthToken {
    symbol: u8,
    extra: u8,
    extra_bits: u8,
}

fn encode_lengths<'a>(lengths: &[u8], tokens: &'a mut [LengthToken]) -> &'a [LengthToken] {
    let mut count = 0;
    let mut emit = |symbol, extra, extra_bits| {
        tokens[count] = LengthToken { symbol, extra, extra_bits };
        count += 1;
    };
    let mut previous = 8; // VP8L's initial previous nonzero length
    for group in lengths.chunk_by(|a, b| a == b) {
        let length = group[0];
        let mut remaining = group.len();
        if length == 0 {
            while remaining >= 11 {
                let repeat = remaining.min(138);
                emit(18, (repeat - 11) as u8, 7);
                remaining -= repeat;
            }
            if remaining >= 3 {
                emit(17, (remaining - 3) as u8, 3);
                remaining = 0;
            }
        } else {
            if length != previous {
                emit(length, 0, 0);
                remaining -= 1;
                previous = length;
            }
            while remaining >= 3 {
                let repeat = remaining.min(6);
                emit(16, (repeat - 3) as u8, 2);
                remaining -= repeat;
            }
        }
        for _ in 0..remaining {
            emit(length, 0, 0);
        }
    }
    &tokens[..count]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(w: usize, h: usize, px: &[[u8; 3]]) -> Vec<u8> {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(px.as_flattened(), w as u32, h as u32, image_webp::ColorType::Rgb8)
            .unwrap();
        out
    }

    fn verify(name: &str, w: usize, h: usize, pixel: impl Fn(usize, usize) -> [u8; 3]) -> (Vec<u8>, usize) {
        let px: Vec<_> = (0..w * h).map(|i| pixel(i % w, i / w)).collect();
        let mut rows = 0;
        let got = encode(w, h, |y, row| {
            assert_eq!(y, rows % h, "{name}");
            rows += 1;
            row.copy_from_slice(&px[y * w..][..w]);
        });
        assert_eq!(rows, h * 2, "{name}");
        let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(&got)).unwrap();
        assert_eq!(decoder.dimensions(), (w as u32, h as u32), "{name}");
        let mut buf = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.read_image(&mut buf).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(buf, px.as_flattened(), "{name}");
        let baseline = reference(w, h, &px).len();
        eprintln!("{name}: {} bytes vs {baseline} with image-webp", got.len());
        (got, baseline)
    }

    const GLYPH: [u8; 9] = [
        0b0011100, 0b0100010, 0b1000001, 0b1000001, 0b1111111, 0b1000001, 0b1000001, 0b1000001, 0b1000001,
    ];

    fn glyph(x: usize, y: usize) -> bool {
        x < 7 && y < GLYPH.len() && (GLYPH[y] >> (6 - x)) & 1 != 0
    }

    fn text(x: usize, y: usize) -> [u8; 3] {
        let (gx, gy) = (x % 10, y % 12);
        if glyph(gx.saturating_sub(1), gy) && gx != 0 {
            [226, 194, 152]
        } else if glyph(gx, gy) || (gx >= 2 && glyph(gx - 2, gy)) {
            // 62 RGB subpixel shades plus foreground and background.
            let shade = ((y / 12 * 19 + x / 10 * 13 + gy * 7) % 62) as u8;
            [40 + shade * 3, 37 + shade * 2, 52 + shade]
        } else {
            [18, 24, 31]
        }
    }

    // Cache bit of this encoder's fixed subtract-green and predictor header.
    fn uses_cache(webp: &[u8]) -> bool {
        const CACHE_FLAG: usize = 78;
        webp[20 + CACHE_FLAG / 8] & (1 << (CACHE_FLAG % 8)) != 0
    }

    #[test]
    fn round_trips_small_and_flat_regions() {
        let (one, _) = verify("1x1", 1, 1, |_, _| [1, 2, 3]);
        assert!(!uses_cache(&one));
        verify("1x5", 1, 5, |_, y| [y as u8, 2, 3]);
        verify("5x1", 5, 1, |x, _| [1, 2, 3 + x as u8]);
        verify("black with runs past 4096", 300, 40, |_, _| [0; 3]);
        verify("solid with runs past 4096", 300, 40, |_, _| [10, 200, 30]);
    }

    #[test]
    fn round_trips_gradients_and_dense_equal_code_lengths() {
        verify("gradient", 256, 7, |x, y| [x as u8, (x + y) as u8, 255 - x as u8]);
        // Every residual channel contains all 256 symbols once. Code length 8
        // is the initial value of repeat code 16, so the header needs no other token.
        verify("all 256 code lengths equal", 256, 1, |x, _| {
            let green = (x * (x + 1) / 2) as u8;
            [green.wrapping_mul(2), green, green.wrapping_mul(2)]
        });
    }

    #[test]
    fn round_trips_noise_without_cache_expansion() {
        let mut state = 0x13a7_9482u32;
        let (w, h) = (77, 33);
        let px: Vec<_> = (0..w * h)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let [r, g, b, _] = state.to_le_bytes();
                [r, g, b]
            })
            .collect();
        let (noise, baseline) = verify("noise, odd size", w, h, |x, y| px[y * w + x]);
        assert!(noise.len() <= baseline * 11 / 10);
        assert!(!uses_cache(&noise));
    }

    #[test]
    fn compresses_text_regions() {
        let (stripes, baseline) = verify("two-color stripes", 64, 64, |x, y| {
            if (x / 3 + y / 5) % 4 == 0 { [20, 30, 40] } else { [250, 245, 240] }
        });
        assert!(stripes.len() < baseline);

        let (mono, baseline) = verify("two-color text", 127, 57, |x, y| {
            if glyph(x % 10, y % 12) { [10, 200, 30] } else { [3, 4, 5] }
        });
        assert!(mono.len() < baseline);

        for colors in [4, 16] {
            verify(&format!("{colors} colors"), 81, 49, |x, y| {
                let c = ((x / 3 + y / 2) % colors) as u8;
                [c * 9, 255 - c * 13, c * 15]
            });
        }

        let (antialiased, baseline) = verify("64-color text", 1920, 1080, text);
        assert!(antialiased.len() < baseline);
        assert!(uses_cache(&antialiased));

        let (tile, baseline) = verify("single-character 64x64 tile", 64, 64, |x, y| {
            if (28..38).contains(&x) && (26..38).contains(&y) {
                text(x - 28, y - 26)
            } else {
                [18, 24, 31]
            }
        });
        assert!(tile.len() < baseline);
    }
}
