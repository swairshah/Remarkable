//! A deliberately small SVG rasterizer: enough to draw the simple diagrams
//! pi tends to produce (boxes, arrows, circles, connecting lines), rendered
//! to a grayscale bitmap we can blit. It supports rect / circle / ellipse /
//! line / polyline / polygon / path (M L H V Z, with curve commands
//! flattened to their endpoints). Anything it can't handle -> returns None,
//! and the caller falls back to showing the SVG source as a code block.
//!
//! Everything renders in black on white; fills are scanline-filled, strokes
//! are drawn as thick segments. No anti-aliasing (e-ink is near-binary).

use crate::text::{self, Face};

struct Shape {
    pts: Vec<(f32, f32)>,
    filled: bool,
    stroke: f32, /* stroke width in user units; 0 = none */
}

enum Anchor {
    Start,
    Middle,
    End,
}

struct TextEl {
    x: f32,
    y: f32, /* SVG baseline */
    size: f32,
    anchor: Anchor,
    content: String,
}

/// Rasterize `src` to (width, height, grayscale bytes), fitting within
/// max_w x max_h. None if there's nothing drawable or parsing fails.
pub fn rasterize(src: &str, max_w: i32, max_h: i32) -> Option<(i32, i32, Vec<u8>)> {
    let mut shapes = Vec::new();
    let mut view: Option<(f32, f32, f32, f32)> = None; /* minx miny w h */

    for tag in tags(src) {
        let name = tag_name(&tag);
        match name {
            "svg" => {
                if let Some(vb) = attr(&tag, "viewBox") {
                    let n = nums(&vb);
                    if n.len() == 4 {
                        view = Some((n[0], n[1], n[2], n[3]));
                    }
                }
            }
            "rect" => {
                let (x, y) = (fattr(&tag, "x"), fattr(&tag, "y"));
                let (w, h) = (fattr(&tag, "width"), fattr(&tag, "height"));
                if w > 0.0 && h > 0.0 {
                    shapes.push(shape_from(
                        &tag,
                        vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
                        true,
                    ));
                }
            }
            "line" => {
                let p = vec![
                    (fattr(&tag, "x1"), fattr(&tag, "y1")),
                    (fattr(&tag, "x2"), fattr(&tag, "y2")),
                ];
                shapes.push(shape_from(&tag, p, false));
            }
            "circle" => {
                let (cx, cy, r) = (fattr(&tag, "cx"), fattr(&tag, "cy"), fattr(&tag, "r"));
                if r > 0.0 {
                    shapes.push(shape_from(&tag, ellipse_pts(cx, cy, r, r), true));
                }
            }
            "ellipse" => {
                let (cx, cy) = (fattr(&tag, "cx"), fattr(&tag, "cy"));
                let (rx, ry) = (fattr(&tag, "rx"), fattr(&tag, "ry"));
                if rx > 0.0 && ry > 0.0 {
                    shapes.push(shape_from(&tag, ellipse_pts(cx, cy, rx, ry), true));
                }
            }
            "polyline" | "polygon" => {
                if let Some(ps) = attr(&tag, "points") {
                    let n = nums(&ps);
                    let pts: Vec<(f32, f32)> = n.chunks_exact(2).map(|c| (c[0], c[1])).collect();
                    if pts.len() >= 2 {
                        shapes.push(shape_from(&tag, pts, name == "polygon"));
                    }
                }
            }
            "path" => {
                if let Some(d) = attr(&tag, "d") {
                    if let Some((pts, closed)) = path_pts(&d) {
                        if pts.len() >= 2 {
                            shapes.push(shape_from(&tag, pts, closed));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let texts = parse_texts(src);
    if shapes.is_empty() && texts.is_empty() {
        return None;
    }

    /* source coordinate bounds: viewBox if given, else the shapes'/text bbox */
    let (mnx, mny, sw, sh) = view.unwrap_or_else(|| {
        let (mut a, mut b, mut c, mut d) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for s in &shapes {
            for &(x, y) in &s.pts {
                a = a.min(x);
                b = b.min(y);
                c = c.max(x);
                d = d.max(y);
            }
        }
        for t in &texts {
            a = a.min(t.x);
            b = b.min(t.y);
            c = c.max(t.x);
            d = d.max(t.y);
        }
        (a, b, c - a, d - b)
    });
    if sw <= 0.0 || sh <= 0.0 {
        return None;
    }

    let pad = 8.0;
    let scale = ((max_w as f32 - 2.0 * pad) / sw).min((max_h as f32 - 2.0 * pad) / sh);
    if !(scale.is_finite() && scale > 0.0) {
        return None;
    }
    let out_w = (sw * scale + 2.0 * pad).ceil() as i32;
    let out_h = (sh * scale + 2.0 * pad).ceil() as i32;
    if out_w < 2 || out_h < 2 || out_w > 1404 || out_h > 1600 {
        return None;
    }
    let mut buf = vec![255u8; (out_w * out_h) as usize];
    let tf = |(x, y): (f32, f32)| ((x - mnx) * scale + pad, (y - mny) * scale + pad);

    /* Only SMALL closed shapes are filled solid (arrowheads, dots, markers);
     * large ones are outlined. Solid-filling a labelled box would bury its
     * text — this is what turned diagrams into black blocks. */
    let area_limit = 0.04 * (out_w as f32) * (out_h as f32);
    for s in &shapes {
        let pts: Vec<(f32, f32)> = s.pts.iter().map(|&p| tf(p)).collect();
        let small = poly_area(&pts) <= area_limit;
        if s.filled && small {
            fill_polygon(&mut buf, out_w, out_h, &pts);
        } else {
            /* outline: closed shapes need the wrap-around segment too */
            let r = ((s.stroke.max(1.0) * scale) / 2.0).round().max(1.0) as i32;
            let n = pts.len();
            let last = if s.filled { n } else { n - 1 };
            for i in 0..last {
                stroke_seg(&mut buf, out_w, out_h, pts[i], pts[(i + 1) % n], r);
            }
        }
    }

    /* text labels, placed by baseline and horizontal anchor */
    for t in &texts {
        if t.content.is_empty() {
            continue;
        }
        let px = (t.size * scale).max(11.0);
        let mut tx = (t.x - mnx) * scale + pad;
        let ty = (t.y - mny) * scale + pad;
        let tw = text::width(Face::Body, px, &t.content) as f32;
        tx -= match t.anchor {
            Anchor::Middle => tw / 2.0,
            Anchor::End => tw,
            Anchor::Start => 0.0,
        };
        let y_top = ty - text::ascent(Face::Body, px);
        text::draw_gray(&mut buf, out_w, out_h, tx as i32, y_top as i32, Face::Body, px, &t.content);
    }
    Some((out_w, out_h, buf))
}

/* ---- shape helpers ------------------------------------------------------- */

fn shape_from(tag: &str, pts: Vec<(f32, f32)>, closed: bool) -> Shape {
    let fill = attr(tag, "fill");
    let has_fill = closed && fill.as_deref() != Some("none");
    let stroke = if attr(tag, "stroke").is_some() {
        let w = fattr(tag, "stroke-width");
        if w > 0.0 { w } else { 1.5 }
    } else if !has_fill {
        1.5 /* nothing else would show it */
    } else {
        0.0
    };
    Shape { pts, filled: has_fill, stroke }
}

/// Absolute polygon area (shoelace), for the small-shape fill test.
fn poly_area(pts: &[(f32, f32)]) -> f32 {
    if pts.len() < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..pts.len() {
        let (x1, y1) = pts[i];
        let (x2, y2) = pts[(i + 1) % pts.len()];
        a += x1 * y2 - x2 * y1;
    }
    (a / 2.0).abs()
}

/// Parse `<text x y font-size text-anchor>content</text>` elements. Nested
/// markup (e.g. <tspan>) inside the content is stripped to plain text.
fn parse_texts(src: &str) -> Vec<TextEl> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(a) = rest.find("<text") {
        let open = &rest[a + 1..];
        let gt = match open.find('>') {
            Some(i) => i,
            None => break,
        };
        let tag = &open[..gt]; /* "text x=.. y=.. …" */
        let content_start = a + 1 + gt + 1;
        let close = match rest[content_start..].find("</text>") {
            Some(i) => i,
            None => break,
        };
        let raw = &rest[content_start..content_start + close];
        let content = strip_tags(raw);
        let size = {
            let s = fattr(tag, "font-size");
            if s > 0.0 {
                s
            } else {
                16.0
            }
        };
        let anchor = match attr(tag, "text-anchor").as_deref() {
            Some("middle") => Anchor::Middle,
            Some("end") => Anchor::End,
            _ => Anchor::Start,
        };
        out.push(TextEl {
            x: fattr(tag, "x"),
            y: fattr(tag, "y"),
            size,
            anchor,
            content,
        });
        rest = &rest[content_start + close + "</text>".len()..];
    }
    out
}

/// Strip any `<...>` markup and collapse whitespace to plain text.
fn strip_tags(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth <= 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn ellipse_pts(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<(f32, f32)> {
    (0..32)
        .map(|i| {
            let a = i as f32 / 32.0 * std::f32::consts::TAU;
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

/// Parse a path `d` into a single point list + whether it closes. Curve
/// commands (C/S/Q/T/A) are approximated by their endpoint.
fn path_pts(d: &str) -> Option<(Vec<(f32, f32)>, bool)> {
    let mut pts = Vec::new();
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let mut closed = false;
    let mut chars = d.char_indices().peekable();
    let bytes = d.as_bytes();
    while let Some((i, c)) = chars.next() {
        if !c.is_ascii_alphabetic() {
            continue;
        }
        let rel = c.is_ascii_lowercase();
        let rest = &d[i + 1..];
        let n = nums(rest);
        let cmd = c.to_ascii_uppercase();
        match cmd {
            'M' | 'L' => {
                for p in n.chunks_exact(2) {
                    let (mut x, mut y) = (p[0], p[1]);
                    if rel {
                        x += cx;
                        y += cy;
                    }
                    cx = x;
                    cy = y;
                    pts.push((x, y));
                }
            }
            'H' => {
                for &v in &n {
                    cx = if rel { cx + v } else { v };
                    pts.push((cx, cy));
                }
            }
            'V' => {
                for &v in &n {
                    cy = if rel { cy + v } else { v };
                    pts.push((cx, cy));
                }
            }
            'Z' => {
                closed = true;
            }
            'C' | 'S' | 'Q' | 'T' | 'A' => {
                /* jump to the command's endpoint (last coordinate pair) */
                if n.len() >= 2 {
                    let (mut x, mut y) = (n[n.len() - 2], n[n.len() - 1]);
                    if rel {
                        x += cx;
                        y += cy;
                    }
                    cx = x;
                    cy = y;
                    pts.push((x, y));
                }
            }
            _ => {}
        }
        let _ = bytes; /* silence unused in some configs */
    }
    if pts.is_empty() {
        None
    } else {
        Some((pts, closed))
    }
}

/* ---- rasterization ------------------------------------------------------- */

fn put(buf: &mut [u8], w: i32, h: i32, x: i32, y: i32) {
    if x >= 0 && x < w && y >= 0 && y < h {
        buf[(y * w + x) as usize] = 0;
    }
}

fn stroke_seg(buf: &mut [u8], w: i32, h: i32, a: (f32, f32), b: (f32, f32), r: i32) {
    let steps = ((b.0 - a.0).abs().max((b.1 - a.1).abs())).ceil().max(1.0) as i32;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = (a.0 + (b.0 - a.0) * t).round() as i32;
        let y = (a.1 + (b.1 - a.1) * t).round() as i32;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    put(buf, w, h, x + dx, y + dy);
                }
            }
        }
    }
}

/// Even-odd scanline fill of a polygon.
fn fill_polygon(buf: &mut [u8], w: i32, h: i32, pts: &[(f32, f32)]) {
    let (mut ymin, mut ymax) = (f32::MAX, f32::MIN);
    for &(_, y) in pts {
        ymin = ymin.min(y);
        ymax = ymax.max(y);
    }
    let y0 = (ymin.floor() as i32).max(0);
    let y1 = (ymax.ceil() as i32).min(h - 1);
    for y in y0..=y1 {
        let yc = y as f32 + 0.5;
        let mut xs = Vec::new();
        for i in 0..pts.len() {
            let (x1, y1) = pts[i];
            let (x2, y2) = pts[(i + 1) % pts.len()];
            if (y1 <= yc && y2 > yc) || (y2 <= yc && y1 > yc) {
                xs.push(x1 + (yc - y1) / (y2 - y1) * (x2 - x1));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for pair in xs.chunks_exact(2) {
            let xa = (pair[0].round() as i32).max(0);
            let xb = (pair[1].round() as i32).min(w - 1);
            for x in xa..=xb {
                put(buf, w, h, x, y);
            }
        }
    }
}

/* ---- tiny XML/number scanning -------------------------------------------- */

/// Yield the inner text of each `<...>` tag (without the angle brackets).
fn tags(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(a) = rest.find('<') {
        if let Some(b) = rest[a..].find('>') {
            out.push(rest[a + 1..a + b].to_string());
            rest = &rest[a + b + 1..];
        } else {
            break;
        }
    }
    out
}

fn tag_name(tag: &str) -> &str {
    tag.trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or("")
}

/// Value of `name="..."` (or `name='...'`) within a tag body.
fn attr(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(p) = tag[from..].find(name) {
        let idx = from + p;
        let after = &tag[idx + name.len()..];
        let after = after.trim_start();
        /* ensure it's this attribute, not a longer name ending in it */
        let before_ok = idx == 0 || !tag.as_bytes()[idx - 1].is_ascii_alphanumeric();
        if before_ok && after.starts_with('=') {
            let after = after[1..].trim_start();
            let q = after.chars().next()?;
            if q == '"' || q == '\'' {
                let end = after[1..].find(q)?;
                return Some(after[1..1 + end].to_string());
            }
        }
        from = idx + name.len();
    }
    None
}

fn fattr(tag: &str, name: &str) -> f32 {
    attr(tag, name).and_then(|s| s.trim().parse().ok()).unwrap_or(0.0)
}

/// Pull all numbers (ints/floats, incl. negatives) out of a string.
fn nums(s: &str) -> Vec<f32> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<f32>| {
        if !cur.is_empty() {
            if let Ok(v) = cur.parse::<f32>() {
                out.push(v);
            }
            cur.clear();
        }
    };
    for c in s.chars() {
        match c {
            '0'..='9' | '.' => cur.push(c),
            '-' => {
                flush(&mut cur, &mut out);
                cur.push('-');
            }
            'e' | 'E' if !cur.is_empty() => cur.push(c),
            _ => flush(&mut cur, &mut out),
        }
    }
    flush(&mut cur, &mut out);
    out
}
