//! SVG → ink: turn the SVG a patch arrives as into pen strokes.
//!
//! Unlike collab's svg.rs (which rasterized diagrams into a box for a chat
//! log), this emits POLYLINES in page coordinates, so a patch is freeform:
//! it can underline the user's words, arrow across the page, or sketch in a
//! margin — and it animates in through the same stamping pipeline as the
//! pen. Everything becomes strokes, including text (Hershey plotter font).
//!
//! Coordinates: no viewBox (or viewBox "0 0 1404 1872") = page pixels,
//! 1:1. Any other viewBox is mapped onto the full page axis-by-axis, so a
//! model that insists on its own units still lands where it intended.
//!
//! Supported: rect, line, circle, ellipse, polyline, polygon, path
//! (M L H V C S Q T A Z — curves flattened, arcs as chords), text
//! (x, y, font-size, text-anchor). Fills: small closed shapes (arrowheads,
//! dots) become scanline hatch strokes; big fills stay outlines so they
//! can't bury the user's writing.

use crate::fb::{SCREEN_H, SCREEN_W};
use crate::hershey;
use crate::ink::{Pt, Stroke, AI_GRAY};

const DEFAULT_R: f32 = 1.6; /* stroke radius ~ a medium nib */
const MAX_R: f32 = 6.0;
const FILL_AREA_MAX: f32 = 9000.0; /* px²: arrowheads yes, boxes no */
const MARGIN: f32 = 30.0; /* text keeps this far from the panel edges */

/// Parse pi's SVG into strokes. `text_scale` is the user's zoom on pi's
/// text (sidebar [-]/[+]), applied to every font-size. The second return
/// is human-readable notes about fixes the converter applied (shrunk or
/// wrapped text etc.) — they go back to pi in the tool result so it
/// learns what actually happened.
pub fn parse(src: &str, text_scale: f32) -> Result<(Vec<Stroke>, Vec<String>), String> {
    let mut polys: Vec<(Vec<(f32, f32)>, f32, bool)> = Vec::new(); /* pts, r, want_fill */
    let mut texts: Vec<TextEl> = Vec::new();
    let mut view: Option<(f32, f32, f32, f32)> = None;

    for tag in tags(src) {
        let name = tag_name(&tag);
        match name {
            "svg" => {
                if let Some(vb) = attr(&tag, "viewBox") {
                    let n = nums(&vb);
                    if n.len() == 4 && n[2] > 0.0 && n[3] > 0.0 {
                        view = Some((n[0], n[1], n[2], n[3]));
                    }
                }
            }
            "rect" => {
                let (x, y) = (fattr(&tag, "x"), fattr(&tag, "y"));
                let (w, h) = (fattr(&tag, "width"), fattr(&tag, "height"));
                if w > 0.0 && h > 0.0 {
                    polys.push((
                        vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h), (x, y)],
                        radius(&tag),
                        wants_fill(&tag),
                    ));
                }
            }
            "line" => polys.push((
                vec![(fattr(&tag, "x1"), fattr(&tag, "y1")), (fattr(&tag, "x2"), fattr(&tag, "y2"))],
                radius(&tag),
                false,
            )),
            "circle" | "ellipse" => {
                let (cx, cy) = (fattr(&tag, "cx"), fattr(&tag, "cy"));
                let (rx, ry) = if name == "circle" {
                    let r = fattr(&tag, "r");
                    (r, r)
                } else {
                    (fattr(&tag, "rx"), fattr(&tag, "ry"))
                };
                if rx > 0.0 && ry > 0.0 {
                    let pts: Vec<(f32, f32)> = (0..=48)
                        .map(|i| {
                            let a = i as f32 / 48.0 * std::f32::consts::TAU;
                            (cx + rx * a.cos(), cy + ry * a.sin())
                        })
                        .collect();
                    polys.push((pts, radius(&tag), wants_fill(&tag)));
                }
            }
            "polyline" | "polygon" => {
                if let Some(ps) = attr(&tag, "points") {
                    let n = nums(&ps);
                    let mut pts: Vec<(f32, f32)> = n.chunks_exact(2).map(|c| (c[0], c[1])).collect();
                    if pts.len() >= 2 {
                        if name == "polygon" {
                            pts.push(pts[0]);
                        }
                        polys.push((pts, radius(&tag), name == "polygon" && wants_fill(&tag)));
                    }
                }
            }
            "path" => {
                if let Some(d) = attr(&tag, "d") {
                    let r = radius(&tag);
                    let fill = wants_fill(&tag);
                    for (pts, closed) in path_polylines(&d) {
                        if pts.len() >= 2 {
                            polys.push((pts, r, closed && fill));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    texts.extend(parse_texts(src));

    if polys.is_empty() && texts.is_empty() {
        return Err("no drawable elements in the SVG".into());
    }

    /* map SVG space onto the page */
    let (vx, vy, kx, ky) = match view {
        Some((x, y, w, h)) => (x, y, SCREEN_W as f32 / w, SCREEN_H as f32 / h),
        None => (0.0, 0.0, 1.0, 1.0),
    };
    let k = kx.min(ky); /* for scalar sizes (radii, font sizes) */
    let tf = |(x, y): (f32, f32)| ((x - vx) * kx, (y - vy) * ky);

    let mut out: Vec<Stroke> = Vec::new();
    let add_poly = |out: &mut Vec<Stroke>, pts: &[(f32, f32)], r: f32| {
        if pts.len() < 2 {
            return;
        }
        out.push(Stroke {
            pts: pts.iter().map(|&p| {
                let (x, y) = tf(p);
                Pt { x, y, r }
            }).collect(),
            gray: AI_GRAY,
        });
    };

    for (pts, r, want_fill) in &polys {
        let r = (r * k).clamp(1.0, MAX_R);
        add_poly(&mut out, pts, r);
        if *want_fill {
            let page_pts: Vec<(f32, f32)> = pts.iter().map(|&p| tf(p)).collect();
            if poly_area(&page_pts) <= FILL_AREA_MAX {
                for row in hatch_rows(&page_pts) {
                    out.push(Stroke {
                        pts: vec![
                            Pt { x: row.0, y: row.2, r: 1.5 },
                            Pt { x: row.1, y: row.2, r: 1.5 },
                        ],
                        gray: AI_GRAY,
                    });
                }
            }
        }
    }

    let mut notes: Vec<String> = Vec::new();

    /* Measure every text element first: when lines of the SAME requested
     * size overflow, shrink the whole size-group uniformly — per-line
     * shrinking made paragraphs ragged (mixed type sizes side by side). */
    const SHRINK_FLOOR: f32 = 0.62;
    struct Measured {
        size: f32,
        x_anchor: f32,
        y: f32,
        max_w: f32,
        full_w: f32,
        runs: Vec<Run>, /* content with LaTeX-lite resolved (sup/sub etc.) */
    }
    let measures: Vec<Option<Measured>> = texts
        .iter()
        .map(|t| {
            if t.content.is_empty() {
                return None;
            }
            let size = (t.size * k * text_scale).clamp(14.0, 140.0);
            let (x_anchor, y) = tf((t.x, t.y));
            let x_anchor = x_anchor.clamp(MARGIN, SCREEN_W as f32 - MARGIN);
            let max_w = match t.anchor {
                Anchor::Start => SCREEN_W as f32 - MARGIN - x_anchor,
                Anchor::End => x_anchor - MARGIN,
                Anchor::Middle => {
                    2.0 * (x_anchor - MARGIN).min(SCREEN_W as f32 - MARGIN - x_anchor)
                }
            }
            .max(size);
            let runs = math_runs(&t.content);
            let full_w = runs_width(t.face, &runs, size);
            Some(Measured { size, x_anchor, y, max_w, full_w, runs })
        })
        .collect();

    let mut group_scale: std::collections::HashMap<i32, f32> = std::collections::HashMap::new();
    for m in measures.iter().flatten() {
        let need = (m.max_w / m.full_w).min(1.0);
        if need < SHRINK_FLOOR {
            continue; /* hopeless outlier: wraps alone, mustn't drag the group */
        }
        let e = group_scale.entry(m.size.round() as i32).or_insert(1.0);
        if need < *e {
            *e = need;
        }
    }
    for (key, sc) in &group_scale {
        if *sc < 0.995 {
            notes.push(format!(
                "all font-size {key} text was shrunk uniformly to {} so its longest \
                 line fits (keep lines shorter to avoid this)",
                (*key as f32 * sc).round(),
            ));
        }
    }

    for (t, m) in texts.iter().zip(&measures) {
        let Some(m) = m else { continue };
        let sc = group_scale.get(&(m.size.round() as i32)).copied().unwrap_or(1.0);
        let is_math = m.runs.len() > 1;
        if m.full_w * sc <= m.max_w || is_math {
            /* one line (formulas never word-wrap; worst case the group /
             * floor scale already shrank them) */
            let size = if m.full_w * sc <= m.max_w {
                m.size * sc
            } else {
                let fit = (m.max_w / m.full_w).max(SHRINK_FLOOR);
                let short: String = t.content.chars().take(28).collect();
                notes.push(format!(
                    "formula \"{short}\u{2026}\" was shrunk to font-size {} to fit its line",
                    (m.size * fit).round(),
                ));
                m.size * fit
            };
            emit_runs(&mut out, t.face, &m.runs, m.x_anchor, m.y, size, &t.anchor);
        } else {
            /* an extreme plain-text line the floor could not save: wrap */
            let sz = (m.size * SHRINK_FLOOR).max(14.0).floor();
            let content = &m.runs[0].text; /* commands already resolved */
            let ls = wrap_text(t.face, content, sz, m.max_w.max(sz));
            let short: String = t.content.chars().take(28).collect();
            notes.push(format!(
                "text \"{short}\u{2026}\" was far too wide ({}px for {}px of room): shrunk \
                 to font-size {sz} AND wrapped into {} lines (baselines every {}px \
                 down to y={}) \u{2014} CHECK the result for collisions with your other \
                 elements and redraw if needed",
                m.full_w.round(),
                m.max_w.round(),
                ls.len(),
                hershey::line_height(sz).round(),
                (m.y + hershey::line_height(sz) * (ls.len() - 1) as f32).round(),
            ));
            for (i, line) in ls.iter().enumerate() {
                let ly = m.y + hershey::line_height(sz) * i as f32;
                let one = vec![Run { text: line.clone(), scale: 1.0, dy: 0.0 }];
                emit_runs(&mut out, t.face, &one, m.x_anchor, ly, sz, &t.anchor);
            }
        }
    }

    /* drop strokes entirely off-page rather than stamping nothing later */
    let before = out.len();
    out.retain(|s| {
        s.pts.iter().any(|p| {
            p.x > -20.0 && p.x < SCREEN_W as f32 + 20.0 && p.y > -20.0 && p.y < SCREEN_H as f32 + 20.0
        })
    });
    if out.len() < before {
        notes.push(format!("{} stroke(s) fell entirely off the page and were dropped", before - out.len()));
    }
    if out.is_empty() {
        return Err("everything in the SVG lies outside the page".into());
    }
    Ok((out, notes))
}

/* ---- LaTeX-lite for page ink ---------------------------------------------- */

/// One styled run of a text line: plain text at a scale/baseline offset.
/// `scale` multiplies the font size; `dy` is in em of the base size
/// (negative = raised superscript).
struct Run {
    text: String,
    scale: f32,
    dy: f32,
}

fn runs_width(face: hershey::Face, runs: &[Run], size: f32) -> f32 {
    runs.iter().map(|r| hershey::text_width(face, &r.text, size * r.scale)).sum()
}

/// Stamp a line of runs as strokes, honoring the anchor over the TOTAL
/// width, advancing x run by run, shifting baselines for super/subscripts.
fn emit_runs(
    out: &mut Vec<Stroke>,
    face: hershey::Face,
    runs: &[Run],
    x_anchor: f32,
    y: f32,
    size: f32,
    anchor: &Anchor,
) {
    let total = runs_width(face, runs, size);
    let mut cx = x_anchor
        - match anchor {
            Anchor::Middle => total / 2.0,
            Anchor::End => total,
            Anchor::Start => 0.0,
        };
    for run in runs {
        let rs = size * run.scale;
        let r = (rs / 24.0).clamp(1.1, 3.0);
        for path in hershey::strokes(face, &run.text, cx, y + size * run.dy, rs) {
            out.push(Stroke {
                pts: path.into_iter().map(|(px, py)| Pt { x: px, y: py, r }).collect(),
                gray: AI_GRAY,
            });
        }
        cx += hershey::text_width(face, &run.text, rs);
    }
}

/// LaTeX command -> unicode (Greek renders via the Hershey Greek face,
/// math symbols via the MATH table; leftovers hit hershey::fold).
fn tex_char(word: &str) -> Option<&'static str> {
    Some(match word {
        "alpha" => "\u{03B1}",
        "beta" => "\u{03B2}",
        "gamma" => "\u{03B3}",
        "delta" => "\u{03B4}",
        "epsilon" | "varepsilon" => "\u{03B5}",
        "zeta" => "\u{03B6}",
        "eta" => "\u{03B7}",
        "theta" => "\u{03B8}",
        "iota" => "\u{03B9}",
        "kappa" => "\u{03BA}",
        "lambda" => "\u{03BB}",
        "mu" => "\u{03BC}",
        "nu" => "\u{03BD}",
        "xi" => "\u{03BE}",
        "pi" => "\u{03C0}",
        "rho" => "\u{03C1}",
        "sigma" => "\u{03C3}",
        "tau" => "\u{03C4}",
        "upsilon" => "\u{03C5}",
        "phi" | "varphi" => "\u{03C6}",
        "chi" => "\u{03C7}",
        "psi" => "\u{03C8}",
        "omega" => "\u{03C9}",
        "Gamma" => "\u{0393}",
        "Delta" => "\u{0394}",
        "Theta" => "\u{0398}",
        "Lambda" => "\u{039B}",
        "Xi" => "\u{039E}",
        "Pi" => "\u{03A0}",
        "Sigma" => "\u{03A3}",
        "Phi" => "\u{03A6}",
        "Psi" => "\u{03A8}",
        "Omega" => "\u{03A9}",
        "approx" => "\u{2248}",
        "sim" => "\u{223C}",
        "cdot" => "\u{00B7}",
        "times" => "\u{00D7}",
        "div" => "\u{00F7}",
        "pm" => "\u{00B1}",
        "mp" => "\u{2213}",
        "le" | "leq" => "\u{2264}",
        "ge" | "geq" => "\u{2265}",
        "ne" | "neq" => "\u{2260}",
        "equiv" => "\u{2261}",
        "infty" => "\u{221E}",
        "to" | "rightarrow" | "mapsto" => "\u{2192}",
        "gets" | "leftarrow" => "\u{2190}",
        "uparrow" => "\u{2191}",
        "downarrow" => "\u{2193}",
        "propto" => "\u{221D}",
        "partial" => "\u{2202}",
        "nabla" => "\u{2207}",
        "sum" => "\u{2211}",
        "prod" => "\u{220F}",
        "int" => "\u{222B}",
        "in" => "\u{2208}",
        "cup" => "\u{222A}",
        "cap" => "\u{2229}",
        "subset" | "subseteq" => "\u{2282}",
        "supset" | "supseteq" => "\u{2283}",
        "forall" => "\u{2200}",
        "exists" => "\u{2203}",
        "emptyset" | "varnothing" => "\u{2205}",
        "perp" => "\u{22A5}",
        "angle" => "\u{2220}",
        "therefore" => "\u{2234}",
        "deg" => "\u{00B0}",
        "left" | "right" | "displaystyle" | "limits" => "",
        "quad" | "qquad" => "  ",
        _ => return None,
    })
}

/// `{...}` group at `i` (or a single char / single command); returns
/// (content, index after).
fn tex_group(b: &[char], i: usize) -> (String, usize) {
    if b.get(i) == Some(&'{') {
        let mut depth = 1;
        let mut j = i + 1;
        let mut g = String::new();
        while j < b.len() && depth > 0 {
            match b[j] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                g.push(b[j]);
            }
            j += 1;
        }
        (g, j)
    } else if b.get(i) == Some(&'\\') {
        let mut j = i + 1;
        let mut w = String::from("\\");
        while j < b.len() && b[j].is_ascii_alphabetic() {
            w.push(b[j]);
            j += 1;
        }
        (w, j)
    } else {
        (b.get(i).map(|c| c.to_string()).unwrap_or_default(), i + 1)
    }
}

/// Resolve a snippet to plain text (commands mapped, structure flattened) —
/// used inside sup/sub groups and \frac parts.
fn tex_flatten(s: &str) -> String {
    math_runs(s).into_iter().map(|r| r.text).collect()
}

/// Literal Unicode super/subscripts (mc², x₀, 10⁻³) rewritten as `^`/`_`
/// so they become true raised/lowered runs instead of flat digits.
fn fold_supsub(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let u = c as u32;
        match u {
            0x00B9 => out.push_str("^1"),
            0x00B2 => out.push_str("^2"),
            0x00B3 => out.push_str("^3"),
            0x2070 => out.push_str("^0"),
            0x2074..=0x2079 => {
                out.push('^');
                out.push((b'0' + (u - 0x2070) as u8) as char);
            }
            0x207A => out.push_str("^+"),
            0x207B => out.push_str("^-"),
            0x207F => out.push_str("^n"),
            0x2080..=0x2089 => {
                out.push('_');
                out.push((b'0' + (u - 0x2080) as u8) as char);
            }
            0x208A => out.push_str("_+"),
            0x208B => out.push_str("_-"),
            _ => out.push(c),
        }
    }
    out
}

/// Split a text line into runs: `$` stripped, LaTeX commands resolved,
/// `^`/`_` becoming true super/subscript runs. Plain lines come back as
/// one untouched run.
fn math_runs(s: &str) -> Vec<Run> {
    let folded;
    let s = if s.chars().any(|c| matches!(c as u32,
        0x00B9 | 0x00B2 | 0x00B3 | 0x2070 | 0x2074..=0x207F | 0x2080..=0x208B))
    {
        folded = fold_supsub(s);
        &folded
    } else {
        s
    };
    if !s.contains(['^', '_', '\\', '$']) {
        return vec![Run { text: s.to_string(), scale: 1.0, dy: 0.0 }];
    }
    let b: Vec<char> = s.chars().collect();
    let mut out: Vec<Run> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '$' => i += 1,
            '\\' => {
                let mut j = i + 1;
                let mut word = String::new();
                while j < b.len() && b[j].is_ascii_alphabetic() {
                    word.push(b[j]);
                    j += 1;
                }
                if word.is_empty() {
                    if let Some(&e) = b.get(j) {
                        buf.push(if ",;!".contains(e) { ' ' } else { e });
                        j += 1;
                    }
                    i = j;
                } else if word == "frac" {
                    let (num, j2) = tex_group(&b, j);
                    let (den, j3) = tex_group(&b, j2);
                    let (num, den) = (tex_flatten(&num), tex_flatten(&den));
                    let wrap_if = |p: &str| {
                        if p.chars().count() > 1 { format!("({p})") } else { p.to_string() }
                    };
                    buf.push_str(&wrap_if(&num));
                    buf.push('/');
                    buf.push_str(&wrap_if(&den));
                    i = j3;
                } else if word == "sqrt" {
                    let (arg, j2) = tex_group(&b, j);
                    buf.push_str("\u{221A}(");
                    buf.push_str(&tex_flatten(&arg));
                    buf.push(')');
                    i = j2;
                } else if matches!(word.as_str(), "text" | "mathrm" | "mathbf" | "operatorname") {
                    let (arg, j2) = tex_group(&b, j);
                    buf.push_str(&arg);
                    i = j2;
                } else {
                    buf.push_str(tex_char(&word).unwrap_or(&word));
                    i = j;
                }
            }
            '^' | '_' => {
                let up = b[i] == '^';
                let (grp, j2) = tex_group(&b, i + 1);
                if !buf.is_empty() {
                    out.push(Run { text: std::mem::take(&mut buf), scale: 1.0, dy: 0.0 });
                }
                out.push(Run {
                    text: tex_flatten(&grp),
                    scale: 0.6,
                    dy: if up { -0.35 } else { 0.18 },
                });
                i = j2;
            }
            '{' | '}' => i += 1,
            c => {
                buf.push(c);
                i += 1;
            }
        }
    }
    if !buf.is_empty() {
        out.push(Run { text: buf, scale: 1.0, dy: 0.0 });
    }
    if out.is_empty() {
        out.push(Run { text: String::new(), scale: 1.0, dy: 0.0 });
    }
    out
}

/// Word-wrap `s` so each line's stroke width fits `max_w` px. A single
/// word wider than the space is hard-broken by characters.
fn wrap_text(face: hershey::Face, s: &str, size: f32, max_w: f32) -> Vec<String> {
    if hershey::text_width(face, s, size) <= max_w {
        return vec![s.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let push_word = |word: &str, lines: &mut Vec<String>, cur: &mut String| {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if hershey::text_width(face, &cand, size) <= max_w {
            *cur = cand;
            return;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(cur));
        }
        if hershey::text_width(face, word, size) <= max_w {
            *cur = word.to_string();
            return;
        }
        /* hard-break an over-wide word by characters */
        let mut piece = String::new();
        for c in word.chars() {
            piece.push(c);
            if hershey::text_width(face, &piece, size) > max_w && piece.chars().count() > 1 {
                piece.pop();
                lines.push(std::mem::take(&mut piece));
                piece.push(c);
            }
        }
        *cur = piece;
    };
    for word in s.split_whitespace() {
        push_word(word, &mut lines, &mut cur);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/* ---- fills --------------------------------------------------------------- */

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

/// Even-odd scanline spans of a polygon, one every ~2.2px — dense enough
/// that 1.5px-radius strokes read as solid.
fn hatch_rows(pts: &[(f32, f32)]) -> Vec<(f32, f32, f32)> {
    let (mut ymin, mut ymax) = (f32::MAX, f32::MIN);
    for &(_, y) in pts {
        ymin = ymin.min(y);
        ymax = ymax.max(y);
    }
    let mut rows = Vec::new();
    let mut y = ymin;
    while y <= ymax {
        let mut xs = Vec::new();
        for i in 0..pts.len() {
            let (x1, y1) = pts[i];
            let (x2, y2) = pts[(i + 1) % pts.len()];
            if (y1 <= y && y2 > y) || (y2 <= y && y1 > y) {
                xs.push(x1 + (y - y1) / (y2 - y1) * (x2 - x1));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for pair in xs.chunks_exact(2) {
            if pair[1] - pair[0] >= 0.5 {
                rows.push((pair[0], pair[1], y));
            }
        }
        y += 2.2;
    }
    rows
}

/* ---- path parsing (full current-point tracking, curves flattened) -------- */

fn path_polylines(d: &str) -> Vec<(Vec<(f32, f32)>, bool)> {
    let mut out: Vec<(Vec<(f32, f32)>, bool)> = Vec::new();
    let mut cur: Vec<(f32, f32)> = Vec::new();
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    let (mut sx, mut sy) = (0.0f32, 0.0f32); /* subpath start, for Z */
    let mut last_ctrl: Option<(f32, f32)> = None; /* for S/T reflection */
    let mut last_cmd = ' ';

    let toks = tokenize_path(d);
    let mut i = 0;
    let flush = |cur: &mut Vec<(f32, f32)>, out: &mut Vec<(Vec<(f32, f32)>, bool)>, closed: bool| {
        if cur.len() >= 2 {
            out.push((std::mem::take(cur), closed));
        } else {
            cur.clear();
        }
    };

    while i < toks.len() {
        let cmd = match &toks[i] {
            Tok::Cmd(c) => {
                i += 1;
                *c
            }
            Tok::Num(_) => {
                /* implicit repeat: M -> L after the first pair */
                match last_cmd {
                    'M' => 'L',
                    'm' => 'l',
                    c => c,
                }
            }
        };
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        let num = |i: &mut usize| -> Option<f32> {
            if let Some(Tok::Num(v)) = toks.get(*i) {
                *i += 1;
                Some(*v)
            } else {
                None
            }
        };
        macro_rules! take {
            () => {
                match num(&mut i) {
                    Some(v) => v,
                    None => break,
                }
            };
        }
        match up {
            'M' => {
                let (mut x, mut y) = (take!(), take!());
                if rel {
                    x += cx;
                    y += cy;
                }
                flush(&mut cur, &mut out, false);
                cx = x;
                cy = y;
                sx = x;
                sy = y;
                cur.push((x, y));
                last_ctrl = None;
            }
            'L' => {
                let (mut x, mut y) = (take!(), take!());
                if rel {
                    x += cx;
                    y += cy;
                }
                cx = x;
                cy = y;
                cur.push((x, y));
                last_ctrl = None;
            }
            'H' => {
                let v = take!();
                cx = if rel { cx + v } else { v };
                cur.push((cx, cy));
                last_ctrl = None;
            }
            'V' => {
                let v = take!();
                cy = if rel { cy + v } else { v };
                cur.push((cx, cy));
                last_ctrl = None;
            }
            'C' | 'S' => {
                let (x1, y1) = if up == 'C' {
                    let (mut a, mut b) = (take!(), take!());
                    if rel {
                        a += cx;
                        b += cy;
                    }
                    (a, b)
                } else {
                    match last_ctrl {
                        Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        None => (cx, cy),
                    }
                };
                let (mut x2, mut y2) = (take!(), take!());
                let (mut x, mut y) = (take!(), take!());
                if rel {
                    x2 += cx;
                    y2 += cy;
                    x += cx;
                    y += cy;
                }
                for s in 1..=16 {
                    let t = s as f32 / 16.0;
                    let u = 1.0 - t;
                    let bx = u * u * u * cx + 3.0 * u * u * t * x1 + 3.0 * u * t * t * x2 + t * t * t * x;
                    let by = u * u * u * cy + 3.0 * u * u * t * y1 + 3.0 * u * t * t * y2 + t * t * t * y;
                    cur.push((bx, by));
                }
                last_ctrl = Some((x2, y2));
                cx = x;
                cy = y;
            }
            'Q' | 'T' => {
                let (x1, y1) = if up == 'Q' {
                    let (mut a, mut b) = (take!(), take!());
                    if rel {
                        a += cx;
                        b += cy;
                    }
                    (a, b)
                } else {
                    match last_ctrl {
                        Some((px, py)) => (2.0 * cx - px, 2.0 * cy - py),
                        None => (cx, cy),
                    }
                };
                let (mut x, mut y) = (take!(), take!());
                if rel {
                    x += cx;
                    y += cy;
                }
                for s in 1..=12 {
                    let t = s as f32 / 12.0;
                    let u = 1.0 - t;
                    let bx = u * u * cx + 2.0 * u * t * x1 + t * t * x;
                    let by = u * u * cy + 2.0 * u * t * y1 + t * t * y;
                    cur.push((bx, by));
                }
                last_ctrl = Some((x1, y1));
                cx = x;
                cy = y;
            }
            'A' => {
                /* rx ry rot large sweep x y — approximated by its chord;
                 * the prompt steers pi to paths/circles instead of arcs */
                let (_rx, _ry, _rot, _laf, _sf) = (take!(), take!(), take!(), take!(), take!());
                let (mut x, mut y) = (take!(), take!());
                if rel {
                    x += cx;
                    y += cy;
                }
                cx = x;
                cy = y;
                cur.push((x, y));
                last_ctrl = None;
            }
            'Z' => {
                cur.push((sx, sy));
                cx = sx;
                cy = sy;
                flush(&mut cur, &mut out, true);
                last_ctrl = None;
            }
            _ => break,
        }
        last_cmd = cmd;
    }
    flush(&mut cur, &mut out, false);
    out
}

enum Tok {
    Cmd(char),
    Num(f32),
}

fn tokenize_path(d: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<Tok>| {
        if !cur.is_empty() {
            if let Ok(v) = cur.parse::<f32>() {
                out.push(Tok::Num(v));
            }
            cur.clear();
        }
    };
    let mut prev = ' ';
    for c in d.chars() {
        match c {
            'a'..='z' | 'A'..='Z' if c != 'e' && c != 'E' => {
                flush(&mut cur, &mut out);
                out.push(Tok::Cmd(c));
            }
            '0'..='9' | '.' => {
                /* a second '.' starts a new number (SVG shorthand "1.5.5") */
                if c == '.' && cur.contains('.') {
                    flush(&mut cur, &mut out);
                }
                cur.push(c);
            }
            '-' | '+' => {
                if prev == 'e' || prev == 'E' {
                    cur.push(c); /* exponent sign */
                } else {
                    flush(&mut cur, &mut out);
                    cur.push(c);
                }
            }
            'e' | 'E' if !cur.is_empty() => cur.push(c),
            _ => flush(&mut cur, &mut out),
        }
        prev = c;
    }
    flush(&mut cur, &mut out);
    out
}

/* ---- attributes / text --------------------------------------------------- */

fn radius(tag: &str) -> f32 {
    let w = fattr(tag, "stroke-width");
    if w > 0.0 {
        (w / 2.0).clamp(1.0, MAX_R)
    } else {
        DEFAULT_R
    }
}

fn wants_fill(tag: &str) -> bool {
    match attr(tag, "fill").as_deref() {
        None => false, /* unspecified: treat as outline (unlike real SVG!) */
        Some("none") | Some("transparent") | Some("white") | Some("#fff") | Some("#ffffff") => false,
        Some(_) => true,
    }
}

enum Anchor {
    Start,
    Middle,
    End,
}

struct TextEl {
    x: f32,
    y: f32,
    size: f32,
    anchor: Anchor,
    face: hershey::Face,
    content: String,
}

fn parse_texts(src: &str) -> Vec<TextEl> {
    let default_face = hershey::default_face();
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(a) = rest.find("<text") {
        let open = &rest[a + 1..];
        let gt = match open.find('>') {
            Some(i) => i,
            None => break,
        };
        let tag = &open[..gt];
        let content_start = a + 1 + gt + 1;
        let close = match rest[content_start..].find("</text>") {
            Some(i) => i,
            None => break,
        };
        let raw = &rest[content_start..content_start + close];
        let size = {
            let s = fattr(tag, "font-size");
            if s > 0.0 {
                s
            } else {
                30.0
            }
        };
        let anchor = match attr(tag, "text-anchor").as_deref() {
            Some("middle") => Anchor::Middle,
            Some("end") => Anchor::End,
            _ => Anchor::Start,
        };
        let face = attr(tag, "font-family")
            .and_then(|f| hershey::face_from_name(&f))
            .unwrap_or(default_face);
        out.push(TextEl {
            x: fattr(tag, "x"),
            y: fattr(tag, "y"),
            size,
            anchor,
            face,
            content: strip_tags(raw),
        });
        rest = &rest[content_start + close + "</text>".len()..];
    }
    out
}

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
    let cleaned = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/* ---- tiny XML/number scanning (shared shape with collab's svg.rs) -------- */

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

fn attr(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(p) = tag[from..].find(name) {
        let idx = from + p;
        let after = &tag[idx + name.len()..];
        let after = after.trim_start();
        let before_ok = idx == 0 || {
            let b = tag.as_bytes()[idx - 1];
            !b.is_ascii_alphanumeric() && b != b'-'
        };
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
    attr(tag, name)
        .and_then(|s| {
            let t = s.trim().trim_end_matches("px");
            t.trim().parse().ok()
        })
        .unwrap_or(0.0)
}

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
