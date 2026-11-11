//! Lossless WebP (VP8L) encoding of opaque images, one row at a time.
//!
//! The bitstream is exactly what image-webp 0.2.4 writes for an RGB image
//! (subtract-green and "pixel above" predictor transforms, one Huffman code
//! per channel, runs of the previous pixel), and the Huffman code below is
//! adapted from it: https://github.com/image-rs/image-webp, by the image-rs
//! developers, MIT or Apache-2.0. image-webp takes the whole image as packed
//! RGBA and copies it once more inside; this asks for each row when it needs
//! it, twice (once to count symbols, once to write them), and keeps only the
//! current and previous row, so a region of the screen needs no copy at all.

use std::collections::BinaryHeap;

/// Encode a `width` x `height` opaque image. `row(y, out)` fills `out` with
/// row `y` as RGB; it is called for every row, top to bottom, twice.
pub fn encode(width: usize, height: usize, mut row: impl FnMut(usize, &mut [[u8; 3]])) -> Vec<u8> {
    assert!((1..=16384).contains(&width) && (1..=16384).contains(&height), "WebP sizes are 1 to 16384");

    // Pass 1: symbol frequencies.
    let mut green = [0u32; 280];
    let mut red = [0u32; 256];
    let mut blue = [0u32; 256];
    residuals(width, height, &mut row, |symbol| match symbol {
        Symbol::Pixel([r, g, b]) => {
            green[g as usize] += 1;
            red[r as usize] += 1;
            blue[b as usize] += 1;
        }
        Symbol::Run(n) => green[256 + run_symbol(n).0 as usize] += 1,
    });

    // RIFF and VP8L chunk headers, sizes filled in at the end.
    let mut w = BitWriter { out: Vec::with_capacity(width * height / 4 + 1024), buffer: 0, nbits: 0 };
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
    write_single_entry_huffman_tree(&mut w, 2);
    for _ in 0..4 {
        write_single_entry_huffman_tree(&mut w, 0);
    }
    w.write_bits(0, 1); // no more transforms
    w.write_bits(0, 1); // no colour cache
    w.write_bits(0, 1); // no meta Huffman codes

    let (mut green_len, mut green_code) = ([0u8; 280], [0u16; 280]);
    let (mut red_len, mut red_code) = ([0u8; 256], [0u16; 256]);
    let (mut blue_len, mut blue_code) = ([0u8; 256], [0u16; 256]);
    write_huffman_tree(&mut w, &green, &mut green_len, &mut green_code);
    write_huffman_tree(&mut w, &red, &mut red_len, &mut red_code);
    write_huffman_tree(&mut w, &blue, &mut blue_len, &mut blue_code);
    write_single_entry_huffman_tree(&mut w, 0); // alpha: always 0 after prediction
    write_single_entry_huffman_tree(&mut w, 1); // distance: the previous pixel

    // Pass 2: the pixels.
    residuals(width, height, &mut row, |symbol| match symbol {
        Symbol::Pixel([r, g, b]) => {
            let (lg, lr, lb) = (green_len[g as usize], red_len[r as usize], blue_len[b as usize]);
            let code = u64::from(green_code[g as usize])
                | u64::from(red_code[r as usize]) << lg
                | u64::from(blue_code[b as usize]) << (lg + lr);
            w.write_bits(code, lg + lr + lb);
        }
        Symbol::Run(n) => {
            let (symbol, extra) = run_symbol(n);
            let s = 256 + symbol as usize;
            w.write_bits(u64::from(green_code[s]), green_len[s]);
            w.write_bits((n as u64 - 1) & ((1 << extra) - 1), extra);
        }
    });
    w.flush();

    let mut out = w.out;
    let frame = out.len() - 20;
    if frame % 2 == 1 {
        out.push(0);
    }
    let riff = out.len() as u32 - 8;
    out[4..8].copy_from_slice(&riff.to_le_bytes());
    out[16..20].copy_from_slice(&(frame as u32).to_le_bytes());
    out
}

enum Symbol {
    /// A pixel after both transforms, as (red, green, blue).
    Pixel([u8; 3]),
    /// That many more copies of the previous pixel (1 to 4096).
    Run(usize),
}

/// The pixels after the transforms, in order, with repeats folded into runs.
fn residuals(width: usize, height: usize, row: &mut impl FnMut(usize, &mut [[u8; 3]]), mut emit: impl FnMut(Symbol)) {
    let mut prev = vec![[0u8; 3]; width];
    let mut cur = vec![[0u8; 3]; width];
    let mut last = None;
    let mut run = 0;
    for y in 0..height {
        row(y, &mut cur);
        for px in cur.iter_mut() {
            *px = [px[0].wrapping_sub(px[1]), px[1], px[2].wrapping_sub(px[1])]; // subtract green
        }
        for x in 0..width {
            // Predict from the pixel above; on the first row from the one to the left.
            let p = if y > 0 { prev[x] } else if x > 0 { cur[x - 1] } else { [0; 3] };
            let c = cur[x];
            let res = [c[0].wrapping_sub(p[0]), c[1].wrapping_sub(p[1]), c[2].wrapping_sub(p[2])];
            if last == Some(res) && run < 4096 {
                run += 1;
                continue;
            }
            if run > 0 {
                emit(Symbol::Run(run));
                run = 0;
            }
            emit(Symbol::Pixel(res));
            last = Some(res);
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

fn write_single_entry_huffman_tree(w: &mut BitWriter, symbol: u8) {
    w.write_bits(1, 2);
    if symbol <= 1 {
        w.write_bits(0, 1);
        w.write_bits(u64::from(symbol), 1);
    } else {
        w.write_bits(1, 1);
        w.write_bits(u64::from(symbol), 8);
    }
}

/// Canonical Huffman code lengths and (bit-reversed) codes, at most
/// `length_limit` bits. False, with everything zero, for one symbol or none.
fn build_huffman_tree(frequencies: &[u32], lengths: &mut [u8], codes: &mut [u16], length_limit: u8) -> bool {
    assert_eq!(frequencies.len(), lengths.len());
    assert_eq!(frequencies.len(), codes.len());

    if frequencies.iter().filter(|&&f| f > 0).count() <= 1 {
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
    let mut internal_nodes = Vec::new();
    let mut nodes = BinaryHeap::from_iter(
        frequencies.iter().enumerate().filter(|(_, f)| **f > 0).map(|(i, &f)| Item(f, i as u16)),
    );
    while nodes.len() > 1 {
        let Item(frequency1, index1) = nodes.pop().unwrap();
        let mut root = nodes.peek_mut().unwrap();
        internal_nodes.push((index1, root.1));
        *root = Item(frequency1 + root.0, internal_nodes.len() as u16 + frequencies.len() as u16 - 1);
    }

    // Walk the tree to assign code lengths.
    lengths.fill(0);
    let mut stack = vec![(nodes.pop().unwrap().1, 0)];
    while let Some((node, depth)) = stack.pop() {
        let node = node as usize;
        if node < frequencies.len() {
            lengths[node] = depth as u8;
        } else {
            let (left, right) = internal_nodes[node - frequencies.len()];
            stack.push((left, depth + 1));
            stack.push((right, depth + 1));
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

    // Assign codes.
    codes.fill(0);
    let mut code = 0u32;
    for len in 1..=length_limit {
        for (i, &length) in lengths.iter().enumerate() {
            if length == len {
                codes[i] = (code as u16).reverse_bits() >> (16 - len);
                code += 1;
            }
        }
        code <<= 1;
    }
    assert_eq!(code, 2 << length_limit);
    true
}

fn write_huffman_tree(w: &mut BitWriter, frequencies: &[u32], lengths: &mut [u8], codes: &mut [u16]) {
    if !build_huffman_tree(frequencies, lengths, codes, 15) {
        let symbol = frequencies.iter().position(|&f| f > 0).unwrap_or(0);
        return write_single_entry_huffman_tree(w, symbol as u8);
    }

    let mut code_length_lengths = [0u8; 16];
    let mut code_length_codes = [0u16; 16];
    let mut code_length_frequencies = [0u32; 16];
    for &length in lengths.iter() {
        code_length_frequencies[length as usize] += 1;
    }
    let single_code_length_length =
        !build_huffman_tree(&code_length_frequencies, &mut code_length_lengths, &mut code_length_codes, 7);

    const CODE_LENGTH_ORDER: [usize; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

    w.write_bits(0, 1); // normal Huffman tree
    w.write_bits(19 - 4, 4); // num_code_lengths - 4
    for i in CODE_LENGTH_ORDER {
        if i > 15 || code_length_frequencies[i] == 0 {
            w.write_bits(0, 3);
        } else if single_code_length_length {
            w.write_bits(1, 3);
        } else {
            w.write_bits(u64::from(code_length_lengths[i]), 3);
        }
    }
    match lengths.len() {
        256 => {
            w.write_bits(1, 1); // max_symbol is stored
            w.write_bits(3, 3); // max_symbol_nbits / 2 - 2
            w.write_bits(254, 8); // max_symbol - 2
        }
        280 => w.write_bits(0, 1),
        _ => unreachable!(),
    }
    if !single_code_length_length {
        for &len in lengths.iter() {
            w.write_bits(u64::from(code_length_codes[len as usize]), code_length_lengths[len as usize]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours(w: usize, h: usize, px: &[[u8; 3]]) -> Vec<u8> {
        encode(w, h, |y, row| row.copy_from_slice(&px[y * w..][..w]))
    }

    fn reference(w: usize, h: usize, px: &[[u8; 3]]) -> Vec<u8> {
        let mut out = Vec::new();
        image_webp::WebPEncoder::new(&mut out)
            .encode(px.as_flattened(), w as u32, h as u32, image_webp::ColorType::Rgb8)
            .unwrap();
        out
    }

    #[test]
    fn same_bytes_as_image_webp_and_decodes_back() {
        let noise = |i: usize| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes();
        let images: Vec<(&str, usize, usize, Box<dyn Fn(usize, usize) -> [u8; 3]>)> = vec![
            ("one pixel", 1, 1, Box::new(|_, _| [1, 2, 3])),
            ("flat, runs past 4096", 300, 40, Box::new(|_, _| [10, 200, 30])),
            ("stripes", 64, 64, Box::new(|x, y| if (x / 3 + y / 5) % 4 == 0 { [20, 30, 40] } else { [250, 245, 240] })),
            ("noise, odd size", 77, 33, Box::new(move |x, y| { let n = noise(x * 131 + y); [n[1], n[2], n[3]] })),
            ("gradient", 256, 7, Box::new(|x, y| [x as u8, (x + y) as u8, 255 - x as u8])),
        ];
        for (name, w, h, f) in images {
            let px: Vec<[u8; 3]> = (0..w * h).map(|i| f(i % w, i / w)).collect();
            let got = ours(w, h, &px);
            assert_eq!(got, reference(w, h, &px), "{name}");
            let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(&got)).unwrap();
            assert_eq!(d.dimensions(), (w as u32, h as u32), "{name}");
            let mut buf = vec![0; d.output_buffer_size().unwrap()];
            d.read_image(&mut buf).unwrap();
            assert_eq!(buf, px.as_flattened(), "{name}");
        }
    }
}
