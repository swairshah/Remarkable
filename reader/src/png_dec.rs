//! Minimal PNG reader for the book bundles: 8-bit grayscale, non-interlaced,
//! any row filter — exactly what tools/mkbook.py (and libpng writers like
//! pymupdf) emit for rendered pages. Includes its own RFC 1950/1951 inflate,
//! so the binary stays dependency-free and fully static.
//!
//! This is a decoder for OUR OWN files, not the open web: anything outside
//! the supported subset is a clean Err, never a panic.

/* ---- inflate (RFC 1951), the classic "puff" shape ------------------------ */

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,  /* next byte */
    bit: u32,    /* bits consumed of data[pos] */
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0, bit: 0 }
    }

    fn bits(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0u32;
        for i in 0..n {
            let byte = *self.data.get(self.pos).ok_or("deflate: out of input")?;
            v |= (((byte >> self.bit) & 1) as u32) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        Ok(v)
    }

    fn align_byte(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }
}

/// Canonical Huffman decoder: count of codes per length + symbols in
/// (length, symbol) order.
struct Huff {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huff {
    fn build(lengths: &[u8]) -> Result<Huff, String> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        /* over-subscribed check */
        let mut left = 1i32;
        for l in 1..16 {
            left = left * 2 - counts[l] as i32;
            if left < 0 {
                return Err("deflate: over-subscribed code".into());
            }
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + counts[l];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Ok(Huff { counts, symbols })
    }

    fn decode(&self, br: &mut BitReader) -> Result<u16, String> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= br.bits(1)? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return Ok(self.symbols[(index + (code - first)) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err("deflate: bad code".into())
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
    131, 163, 195, 227, 258,
];
const LEN_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
    13, 13,
];

fn inflate_block(
    br: &mut BitReader,
    out: &mut Vec<u8>,
    lit: &Huff,
    dist: &Huff,
) -> Result<(), String> {
    loop {
        let sym = lit.decode(br)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            257..=285 => {
                let i = (sym - 257) as usize;
                let len = LEN_BASE[i] as usize + br.bits(LEN_EXTRA[i])? as usize;
                let dsym = dist.decode(br)? as usize;
                if dsym >= 30 {
                    return Err("deflate: bad distance code".into());
                }
                let d = DIST_BASE[dsym] as usize + br.bits(DIST_EXTRA[dsym])? as usize;
                if d > out.len() {
                    return Err("deflate: distance past start".into());
                }
                let start = out.len() - d;
                for k in 0..len {
                    let b = out[start + k];
                    out.push(b);
                }
            }
            _ => return Err("deflate: bad literal/length".into()),
        }
    }
}

/// Inflate a raw DEFLATE stream. `hint` pre-allocates the output.
pub fn inflate(data: &[u8], hint: usize) -> Result<Vec<u8>, String> {
    let mut br = BitReader::new(data);
    let mut out = Vec::with_capacity(hint);
    loop {
        let last = br.bits(1)?;
        match br.bits(2)? {
            0 => {
                /* stored */
                br.align_byte();
                if br.pos + 4 > data.len() {
                    return Err("deflate: truncated stored block".into());
                }
                let len = u16::from_le_bytes([data[br.pos], data[br.pos + 1]]) as usize;
                br.pos += 4; /* LEN + NLEN */
                if br.pos + len > data.len() {
                    return Err("deflate: stored block past end".into());
                }
                out.extend_from_slice(&data[br.pos..br.pos + len]);
                br.pos += len;
            }
            1 => {
                /* fixed codes */
                let mut ll = [0u8; 288];
                for (i, l) in ll.iter_mut().enumerate() {
                    *l = match i {
                        0..=143 => 8,
                        144..=255 => 9,
                        256..=279 => 7,
                        _ => 8,
                    };
                }
                let lit = Huff::build(&ll)?;
                let dist = Huff::build(&[5u8; 30])?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            2 => {
                /* dynamic codes */
                let hlit = br.bits(5)? as usize + 257;
                let hdist = br.bits(5)? as usize + 1;
                let hclen = br.bits(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut cl = [0u8; 19];
                for &o in ORDER.iter().take(hclen) {
                    cl[o] = br.bits(3)? as u8;
                }
                let clh = Huff::build(&cl)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < lengths.len() {
                    let s = clh.decode(&mut br)?;
                    match s {
                        0..=15 => {
                            lengths[i] = s as u8;
                            i += 1;
                        }
                        16 => {
                            if i == 0 {
                                return Err("deflate: repeat at start".into());
                            }
                            let prev = lengths[i - 1];
                            let n = 3 + br.bits(2)? as usize;
                            for _ in 0..n {
                                if i >= lengths.len() {
                                    return Err("deflate: repeat overflow".into());
                                }
                                lengths[i] = prev;
                                i += 1;
                            }
                        }
                        17 | 18 => {
                            let n = if s == 17 {
                                3 + br.bits(3)? as usize
                            } else {
                                11 + br.bits(7)? as usize
                            };
                            if i + n > lengths.len() {
                                return Err("deflate: zero-repeat overflow".into());
                            }
                            i += n;
                        }
                        _ => return Err("deflate: bad code-length symbol".into()),
                    }
                }
                if lengths[256] == 0 {
                    return Err("deflate: no end-of-block code".into());
                }
                let lit = Huff::build(&lengths[..hlit])?;
                let dist = Huff::build(&lengths[hlit..])?;
                inflate_block(&mut br, &mut out, &lit, &dist)?;
            }
            _ => return Err("deflate: bad block type".into()),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

/// Unwrap a zlib (RFC 1950) stream and inflate it.
pub fn zlib_decompress(data: &[u8], hint: usize) -> Result<Vec<u8>, String> {
    if data.len() < 6 {
        return Err("zlib: too short".into());
    }
    if data[0] & 0x0F != 8 {
        return Err("zlib: not deflate".into());
    }
    if data[1] & 0x20 != 0 {
        return Err("zlib: preset dictionary unsupported".into());
    }
    inflate(&data[2..data.len() - 4], hint) /* trailer = adler32, unchecked */
}

/* ---- PNG ------------------------------------------------------------------ */

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Decode an 8-bit grayscale, non-interlaced PNG into (w, h, gray bytes).
pub fn decode_png_gray(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    if data.len() < 8 || &data[..8] != b"\x89PNG\r\n\x1a\n" {
        return Err("png: bad signature".into());
    }
    let mut pos = 8usize;
    let (mut w, mut h) = (0u32, 0u32);
    let mut seen_ihdr = false;
    let mut idat: Vec<u8> = Vec::new();
    while pos + 8 <= data.len() {
        let len = be32(&data[pos..]) as usize;
        let tag = &data[pos + 4..pos + 8];
        let body_start = pos + 8;
        if body_start + len + 4 > data.len() {
            return Err("png: truncated chunk".into());
        }
        let body = &data[body_start..body_start + len];
        match tag {
            b"IHDR" => {
                if len < 13 {
                    return Err("png: short IHDR".into());
                }
                w = be32(&body[0..]);
                h = be32(&body[4..]);
                let (depth, color, ilace) = (body[8], body[9], body[12]);
                if depth != 8 || color != 0 {
                    return Err(format!("png: unsupported format (depth {depth}, color {color}) — books are 8-bit gray"));
                }
                if ilace != 0 {
                    return Err("png: interlaced unsupported".into());
                }
                seen_ihdr = true;
            }
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {} /* ancillary chunks: skip */
        }
        pos = body_start + len + 4; /* skip crc */
    }
    if !seen_ihdr || w == 0 || h == 0 {
        return Err("png: missing/empty IHDR".into());
    }
    if (w as usize).saturating_mul(h as usize) > 16 << 20 {
        return Err("png: unreasonably large".into());
    }
    let stride = w as usize;
    let raw = zlib_decompress(&idat, (stride + 1) * h as usize)?;
    if raw.len() != (stride + 1) * h as usize {
        return Err(format!("png: wrong data size ({} for {w}x{h})", raw.len()));
    }
    let mut out = vec![0u8; stride * h as usize];
    for y in 0..h as usize {
        let filt = raw[y * (stride + 1)];
        let row = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x > 0 { out[y * stride + x - 1] } else { 0 } as i32; /* left */
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 } as i32; /* up */
            let c = if x > 0 && y > 0 { out[(y - 1) * stride + x - 1] } else { 0 } as i32;
            let v = row[x] as i32;
            out[y * stride + x] = match filt {
                0 => v,
                1 => v + a,
                2 => v + b,
                3 => v + (a + b) / 2,
                4 => {
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    v + if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                f => return Err(format!("png: bad filter {f}")),
            } as u8;
        }
    }
    Ok((w, h, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Round-trip against Python's zlib + a reference PNG filterer: real
    /// dynamic-huffman streams and all five row filters.
    #[test]
    fn python_reference_roundtrip() {
        let script = r#"
import sys, zlib, struct, random
random.seed(7)
W, H = 211, 137
img = bytearray()
for y in range(H):
    for x in range(W):
        v = 255 if (x // 7 + y // 5) % 2 else (x * y) % 251
        img.append(v)
raw = bytearray()
for y in range(H):
    f = y % 5
    raw.append(f)
    for x in range(W):
        v = img[y*W+x]; a = img[y*W+x-1] if x else 0
        b = img[(y-1)*W+x] if y else 0; c = img[(y-1)*W+x-1] if x and y else 0
        if f == 0: e = v
        elif f == 1: e = (v - a) & 255
        elif f == 2: e = (v - b) & 255
        elif f == 3: e = (v - (a + b) // 2) & 255
        else:
            p = a + b - c
            pa, pb, pc = abs(p-a), abs(p-b), abs(p-c)
            pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
            e = (v - pr) & 255
        raw.append(e)
def chunk(tag, data):
    c = tag + data
    return struct.pack(">I", len(data)) + c + struct.pack(">I", zlib.crc32(c))
png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 0, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))
sys.stdout.buffer.write(struct.pack(">I", len(png)) + png + bytes(img))
"#;
        let out = Command::new("python3").arg("-c").arg(script).output().expect("python3");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let n = be32(&out.stdout) as usize;
        let (png, want) = out.stdout[4..].split_at(n);
        let (w, h, got) = decode_png_gray(png).expect("decode");
        assert_eq!((w, h), (211, 137));
        assert_eq!(got, want);
    }
}
