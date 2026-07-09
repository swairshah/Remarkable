//! Minimal 8-bit grayscale PNG encoder + base64, no dependencies.
//!
//! The zlib stream inside the PNG uses STORED (uncompressed) deflate
//! blocks — perfectly valid PNG, ~1 byte/pixel. Handwriting snapshots are
//! a few hundred KB; they only travel over a local pipe to pi, which
//! re-encodes for the model API, so compression buys nothing here.

fn crc32(data: &[u8]) -> u32 {
    let mut c: u32 = !0;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

fn chunk(out: &mut Vec<u8>, tag: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(tag);
    out.extend_from_slice(data);
    let mut crc_input = tag.to_vec();
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
}

/// `gray` is row-major, w*h bytes, 0 = black.
pub fn encode_gray(w: u32, h: u32, gray: &[u8]) -> Vec<u8> {
    /* raw scanlines: each row prefixed with filter byte 0 (None) */
    let mut raw = Vec::with_capacity((w as usize + 1) * h as usize);
    for row in gray.chunks_exact(w as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }

    /* zlib: header, then STORED deflate blocks of <=65535 bytes, adler32 */
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    while let Some(block) = blocks.next() {
        z.push(if blocks.peek().is_none() { 1 } else { 0 }); /* BFINAL */
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); /* 8-bit grayscale */

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    png
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(B64[(n >> 18 & 63) as usize] as char);
        out.push(B64[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 { B64[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if c.len() > 2 { B64[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Decode standard base64 (ignores whitespace, stops at padding). Used to
/// reload stored handwriting from the local history file.
pub fn base64_decode(s: &str) -> Vec<u8> {
    let mut inv = [255u8; 256];
    for (i, &c) in B64.iter().enumerate() {
        inv[c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &c in s.as_bytes() {
        if c == b'=' {
            break;
        }
        let v = inv[c as usize];
        if v == 255 {
            continue; /* skip newlines / stray chars */
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}
