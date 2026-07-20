//! Markdown → styled, wrapped, paginated lines for the library reader.
//!
//! Modeled on the user's article-export style (Reader-serif body, semibold
//! headings, `::: aside` blocks as a left-ruled pale box in slightly
//! smaller type, code in a mono block, display math centered): the same
//! ideas mapped onto the three embedded faces — EB Garamond (Body),
//! EB Garamond SemiBold (Heading), Google Sans Code (Mono).
//!
//! Supported: YAML frontmatter (title only), #..#### headings, paragraphs
//! with **bold** / `code` / $math$ / [links] (underlined, URL dropped),
//! ::: aside|note|concept|difficult ::: blocks, > quotes, - and 1. lists
//! with hanging indents, ``` fences, | tables (mono), $$ display math
//! (centered), --- rules. Inline HTML is stripped.

use crate::text::{self, Face};

pub const BODY_PX: f32 = 38.0;
const H_PX: [f32; 4] = [54.0, 46.0, 41.0, 38.0];
const ASIDE_PX: f32 = 34.0;
const CODE_PX: f32 = 27.0;
const TABLE_PX: f32 = 25.0;
const MATH_PX: f32 = 33.0;
const LIST_INDENT: i32 = 54;
const ASIDE_INDENT: i32 = 34;

pub struct Span {
    pub text: String,
    pub face: Face,
    pub px: f32,
    pub underline: bool,
    pub dy: i32, /* baseline shift: negative = superscript */
}

pub struct RLine {
    pub spans: Vec<Span>,
    pub x: i32,     /* extra left indent */
    pub h: i32,     /* advance height */
    pub aside: bool, /* left rule + pale wash */
    pub code: bool,  /* pale code background */
    pub center: bool,
    pub hr: bool,
}

fn line_of(spans: Vec<Span>, x: i32, aside: bool, code: bool, center: bool) -> RLine {
    let px = spans.iter().map(|s| s.px as i32).max().unwrap_or(BODY_PX as i32) as f32;
    RLine { spans, x, h: (px * 1.42) as i32, aside, code, center, hr: false }
}

fn spacer(h: i32) -> RLine {
    RLine { spans: Vec::new(), x: 0, h, aside: false, code: false, center: false, hr: false }
}

/// Lay `md` out to `width` px. Returns (frontmatter title, lines).
pub fn layout(md: &str, width: i32) -> (Option<String>, Vec<RLine>) {
    let mut out: Vec<RLine> = Vec::new();
    let mut title = None;
    let mut lines = md.lines().peekable();

    /* frontmatter */
    if md.starts_with("---") {
        lines.next();
        for l in lines.by_ref() {
            if l.trim() == "---" {
                break;
            }
            if let Some(t) = l.strip_prefix("title:") {
                title = Some(t.trim().trim_matches('"').to_string());
            }
        }
    }

    let mut in_code = false;
    let mut in_aside = false;
    let mut in_math = false;

    for raw in lines {
        let line = raw.trim_end();

        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            out.push(spacer(8));
            continue;
        }
        if in_code {
            for chunk in hard_wrap_mono(line, width - 24, CODE_PX) {
                out.push(line_of(
                    vec![Span { text: chunk, face: Face::Mono, px: CODE_PX, underline: false, dy: 0 }],
                    12,
                    false,
                    true,
                    false,
                ));
            }
            continue;
        }

        let t = line.trim_start();
        if t.starts_with(":::") {
            in_aside = !t[3..].trim().is_empty() || !in_aside;
            if t[3..].trim().is_empty() {
                in_aside = false;
            }
            out.push(spacer(10));
            continue;
        }
        if t == "$$" {
            in_math = !in_math;
            continue;
        }
        if in_math {
            out.push(line_of(math_spans(t, Face::Body, MATH_PX), 0, false, false, true));
            continue;
        }

        if t.is_empty() {
            out.push(spacer(14));
            continue;
        }
        if t == "---" || t == "***" || t == "___" {
            out.push(RLine { spans: Vec::new(), x: 0, h: 26, aside: false, code: false, center: false, hr: true });
            continue;
        }

        /* headings */
        if let Some(rest) = t.strip_prefix('#').map(|r| {
            let mut level = 1;
            let mut r = r;
            while let Some(r2) = r.strip_prefix('#') {
                level += 1;
                r = r2;
            }
            (level.min(4), r.trim_start())
        }) {
            let (level, txt) = rest;
            if !txt.is_empty() && t.starts_with('#') {
                let txt = strip_anchor(txt);
                out.push(spacer(if level <= 2 { 26 } else { 16 }));
                for w in wrap_spans(
                    parse_inline(&txt, Face::Heading, H_PX[level - 1]),
                    width,
                    0,
                ) {
                    out.push(line_of(w, 0, false, false, false));
                }
                out.push(spacer(10));
                continue;
            }
        }

        /* tables (mono, raw-ish) */
        if t.starts_with('|') {
            let cells = t.trim_matches('|').replace('|', "   ");
            if cells.chars().all(|c| "-: ".contains(c)) {
                continue; /* separator row */
            }
            let clean = strip_inline_noise(&cells);
            for chunk in hard_wrap_mono(&clean, width - 24, TABLE_PX) {
                out.push(line_of(
                    vec![Span { text: chunk, face: Face::Mono, px: TABLE_PX, underline: false, dy: 0 }],
                    12,
                    false,
                    false,
                    false,
                ));
            }
            continue;
        }

        /* aside / blockquote content */
        let (t, aside_here) = match t.strip_prefix("> ") {
            Some(rest) => (rest, true),
            None => (t, in_aside),
        };

        let (body_px, indent) = if aside_here { (ASIDE_PX, ASIDE_INDENT) } else { (BODY_PX, 0) };

        /* lists: hanging indent, marker kept */
        let (marker, rest) = split_list_marker(t);
        if let Some(m) = marker {
            let spans = parse_inline(rest, Face::Body, body_px);
            let wrapped = wrap_spans(spans, width - indent - LIST_INDENT, 0);
            for (i, mut w) in wrapped.into_iter().enumerate() {
                let x = if i == 0 {
                    w.insert(0, Span { text: format!("{m} "), face: Face::Body, px: body_px, underline: false, dy: 0 });
                    indent
                } else {
                    indent + LIST_INDENT
                };
                out.push(line_of(w, x, aside_here, false, false));
            }
            continue;
        }

        /* plain paragraph line */
        for w in wrap_spans(parse_inline(t, Face::Body, body_px), width - indent, 0) {
            out.push(line_of(w, indent, aside_here, false, false));
        }
    }
    (title, out)
}

/// Pack lines into pages of at most `page_h` tall; returns index ranges.
pub fn paginate(lines: &[RLine], page_h: i32) -> Vec<(usize, usize)> {
    let mut pages = Vec::new();
    let mut start = 0;
    let mut h = 0;
    for (i, l) in lines.iter().enumerate() {
        if h + l.h > page_h && i > start {
            pages.push((start, i));
            start = i;
            h = 0;
        }
        h += l.h;
    }
    if start < lines.len() || pages.is_empty() {
        pages.push((start, lines.len()));
    }
    pages
}

/* ---- inline parsing ------------------------------------------------------- */

fn strip_anchor(s: &str) -> String {
    match s.find("{#") {
        Some(i) => s[..i].trim_end().to_string(),
        None => s.to_string(),
    }
}

/// Remove html tags and collapse whitespace (for table cells).
fn strip_inline_noise(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a line's inline markup into styled spans.
fn parse_inline(s: &str, base_face: Face, px: f32) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    let mut cur = String::new();
    let bytes: Vec<char> = strip_inline_noise(s).chars().collect();
    let mut i = 0;
    let flush = |cur: &mut String, out: &mut Vec<Span>| {
        if !cur.is_empty() {
            out.push(Span { text: std::mem::take(cur), face: base_face, px, underline: false, dy: 0 });
        }
    };
    while i < bytes.len() {
        let c = bytes[i];
        /* **bold** */
        if c == '*' && i + 1 < bytes.len() && bytes[i + 1] == '*' {
            if let Some(end) = find_seq(&bytes, i + 2, &['*', '*']) {
                flush(&mut cur, &mut out);
                let inner: String = bytes[i + 2..end].iter().collect();
                out.push(Span { text: inner, face: Face::Heading, px, underline: false, dy: 0 });
                i = end + 2;
                continue;
            }
        }
        /* `code` */
        if c == '`' {
            if let Some(end) = bytes[i + 1..].iter().position(|&x| x == '`') {
                flush(&mut cur, &mut out);
                let inner: String = bytes[i + 1..i + 1 + end].iter().collect();
                out.push(Span { text: inner, face: Face::Mono, px: px * 0.82, underline: false, dy: 0 });
                i = i + 2 + end;
                continue;
            }
        }
        /* $math$ */
        if c == '$' {
            if let Some(end) = bytes[i + 1..].iter().position(|&x| x == '$') {
                flush(&mut cur, &mut out);
                let inner: String = bytes[i + 1..i + 1 + end].iter().collect();
                out.extend(math_spans(&inner, base_face, px));
                i = i + 2 + end;
                continue;
            }
        }
        /* ![image](url) and [text](url) */
        if c == '!' && i + 1 < bytes.len() && bytes[i + 1] == '[' {
            if let Some((label, after)) = parse_link(&bytes, i + 1) {
                flush(&mut cur, &mut out);
                out.push(Span { text: format!("[image: {label}]"), face: Face::Mono, px: px * 0.8, underline: false, dy: 0 });
                i = after;
                continue;
            }
        }
        if c == '[' {
            if let Some((label, after)) = parse_link(&bytes, i) {
                flush(&mut cur, &mut out);
                out.push(Span { text: label, face: base_face, px, underline: true, dy: 0 });
                i = after;
                continue;
            }
        }
        /* *italic* -> plain (no italic face embedded) */
        if c == '*' {
            i += 1;
            continue;
        }
        cur.push(c);
        i += 1;
    }
    flush(&mut cur, &mut out);
    out
}

/* ---- math: LaTeX -> styled spans ------------------------------------------ */

fn sp(text: String, face: Face, px: f32) -> Span {
    Span { text, face, px, underline: false, dy: 0 }
}

/// A LaTeX symbol command -> display text, preferring the real glyph when
/// the face has it, else a readable ASCII fallback.
fn tex_symbol(word: &str, face: Face) -> String {
    let (ch, fallback): (char, &str) = match word {
        "approx" => ('\u{2248}', "~"),
        "sim" => ('\u{223C}', "~"),
        "cdot" => ('\u{00B7}', "*"),
        "times" => ('\u{00D7}', "x"),
        "pm" => ('\u{00B1}', "+/-"),
        "mp" => ('\u{2213}', "-/+"),
        "le" | "leq" => ('\u{2264}', "<="),
        "ge" | "geq" => ('\u{2265}', ">="),
        "ne" | "neq" => ('\u{2260}', "!="),
        "infty" => ('\u{221E}', "inf"),
        "sum" => ('\u{03A3}', "sum"),
        "prod" => ('\u{03A0}', "prod"),
        "int" => ('\u{222B}', "integral"),
        "partial" => ('\u{2202}', "d"),
        "nabla" => ('\u{2207}', "grad"),
        "propto" => ('\u{221D}', "prop-to"),
        "in" => ('\u{2208}', "in"),
        "to" | "rightarrow" => ('\u{2192}', "->"),
        "leftarrow" => ('\u{2190}', "<-"),
        "Rightarrow" | "implies" => ('\u{21D2}', "=>"),
        "ell" => ('\u{2113}', "l"),
        "prime" => ('\u{2032}', "'"),
        "dots" | "ldots" | "cdots" => ('\u{2026}', "..."),
        "alpha" => ('\u{03B1}', "alpha"),
        "beta" => ('\u{03B2}', "beta"),
        "gamma" => ('\u{03B3}', "gamma"),
        "delta" => ('\u{03B4}', "delta"),
        "epsilon" | "varepsilon" => ('\u{03B5}', "eps"),
        "zeta" => ('\u{03B6}', "zeta"),
        "eta" => ('\u{03B7}', "eta"),
        "theta" => ('\u{03B8}', "theta"),
        "iota" => ('\u{03B9}', "iota"),
        "kappa" => ('\u{03BA}', "kappa"),
        "lambda" => ('\u{03BB}', "lambda"),
        "mu" => ('\u{03BC}', "mu"),
        "nu" => ('\u{03BD}', "nu"),
        "xi" => ('\u{03BE}', "xi"),
        "pi" => ('\u{03C0}', "pi"),
        "rho" => ('\u{03C1}', "rho"),
        "sigma" => ('\u{03C3}', "sigma"),
        "tau" => ('\u{03C4}', "tau"),
        "upsilon" => ('\u{03C5}', "u"),
        "phi" | "varphi" => ('\u{03C6}', "phi"),
        "chi" => ('\u{03C7}', "chi"),
        "psi" => ('\u{03C8}', "psi"),
        "omega" => ('\u{03C9}', "omega"),
        "Gamma" => ('\u{0393}', "Gamma"),
        "Delta" => ('\u{0394}', "Delta"),
        "Theta" => ('\u{0398}', "Theta"),
        "Lambda" => ('\u{039B}', "Lambda"),
        "Xi" => ('\u{039E}', "Xi"),
        "Pi" => ('\u{03A0}', "Pi"),
        "Sigma" => ('\u{03A3}', "Sigma"),
        "Phi" => ('\u{03A6}', "Phi"),
        "Psi" => ('\u{03A8}', "Psi"),
        "Omega" => ('\u{03A9}', "Omega"),
        "left" | "right" | "displaystyle" | "limits" => return String::new(),
        "quad" | "qquad" => return "  ".into(),
        _ => return word.to_string(), /* unknown command: show its name */
    };
    if text::has_glyph(face, ch) {
        ch.to_string()
    } else {
        fallback.to_string()
    }
}

/// `{...}` group starting at `i` (or a single token when unbraced);
/// returns (content, index after).
fn brace_group(b: &[char], i: usize) -> (String, usize) {
    if b.get(i) == Some(&'{') {
        let mut depth = 1;
        let mut j = i + 1;
        let mut out = String::new();
        while j < b.len() && depth > 0 {
            match b[j] {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth > 0 {
                out.push(b[j]);
            }
            j += 1;
        }
        (out, j)
    } else if b.get(i) == Some(&'\\') {
        /* a single command like ^\infty */
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

/// Render a LaTeX snippet as styled spans: symbols/Greek via real glyphs,
/// ^ and _ as true super/subscripts (smaller, baseline-shifted), \frac as
/// a slash, in the serif face — no equation layout, but honest math-ish
/// typography for the level of math notes contain.
fn math_spans(src: &str, face: Face, px: f32) -> Vec<Span> {
    let b: Vec<char> = src.chars().collect();
    let mut out: Vec<Span> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                out.push(sp(std::mem::take(&mut buf), face, px));
            }
        };
    }
    while i < b.len() {
        match b[i] {
            '\\' => {
                let mut j = i + 1;
                let mut word = String::new();
                while j < b.len() && b[j].is_ascii_alphabetic() {
                    word.push(b[j]);
                    j += 1;
                }
                if word.is_empty() {
                    /* escaped char / spacing: \, \; \! \{ ... */
                    if let Some(&e) = b.get(j) {
                        buf.push(if ",;!".contains(e) { ' ' } else { e });
                        j += 1;
                    }
                    i = j;
                    continue;
                }
                match word.as_str() {
                    "frac" => {
                        let (num, j2) = brace_group(&b, j);
                        let (den, j3) = brace_group(&b, j2);
                        flush!();
                        let wrap_if = |s: &str| {
                            if s.chars().count() > 1 && !s.starts_with('\\') {
                                format!("({s})")
                            } else {
                                s.to_string()
                            }
                        };
                        out.extend(math_spans(&wrap_if(&num), face, px));
                        out.push(sp("/".into(), face, px));
                        out.extend(math_spans(&wrap_if(&den), face, px));
                        i = j3;
                    }
                    "sqrt" => {
                        let (arg, j2) = brace_group(&b, j);
                        flush!();
                        let root = if text::has_glyph(face, '\u{221A}') { "\u{221A}(" } else { "sqrt(" };
                        out.push(sp(root.into(), face, px));
                        out.extend(math_spans(&arg, face, px));
                        out.push(sp(")".into(), face, px));
                        i = j2;
                    }
                    "text" | "mathrm" | "mathbf" | "operatorname" => {
                        let (arg, j2) = brace_group(&b, j);
                        buf.push_str(&arg);
                        i = j2;
                    }
                    _ => {
                        buf.push_str(&tex_symbol(&word, face));
                        i = j;
                    }
                }
            }
            '^' | '_' => {
                let up = b[i] == '^';
                let (grp, j2) = brace_group(&b, i + 1);
                flush!();
                let small = px * 0.62;
                let dy = if up { -(px * 0.32) as i32 } else { (px * 0.22) as i32 };
                for mut s2 in math_spans(&grp, face, small) {
                    s2.dy += dy;
                    out.push(s2);
                }
                i = j2;
            }
            '{' | '}' => i += 1,
            '~' => {
                buf.push(' ');
                i += 1;
            }
            c => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush!();
    if out.is_empty() {
        out.push(sp(String::new(), face, px));
    }
    out
}

fn find_seq(b: &[char], from: usize, seq: &[char]) -> Option<usize> {
    (from..b.len().saturating_sub(seq.len() - 1)).find(|&i| b[i..i + seq.len()] == *seq)
}

/// `[label](url)` at `i` (which points at '['): returns (label, index after).
fn parse_link(b: &[char], i: usize) -> Option<(String, usize)> {
    let close = find_seq(b, i + 1, &[']'])?;
    if close + 1 >= b.len() || b[close + 1] != '(' {
        return None;
    }
    let paren = find_seq(b, close + 2, &[')'])?;
    Some((b[i + 1..close].iter().collect(), paren + 1))
}

fn split_list_marker(t: &str) -> (Option<String>, &str) {
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return (Some("-".into()), rest);
        }
    }
    /* "12. text" */
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        if let Some(rest) = t[digits.len()..].strip_prefix(". ") {
            return (Some(format!("{digits}.")), rest);
        }
    }
    (None, t)
}

/* ---- wrapping ------------------------------------------------------------- */

/// Greedy word-wrap across styled spans.
fn wrap_spans(spans: Vec<Span>, width: i32, _indent: i32) -> Vec<Vec<Span>> {
    let mut lines: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut cur_w = 0i32;
    for sp in spans {
        for (wi, word) in sp.text.split(' ').enumerate() {
            if word.is_empty() {
                continue;
            }
            let piece = if wi == 0 && !cur.is_empty() && matches!(cur.last(), Some(l) if !l.text.ends_with(' '))
            {
                word.to_string()
            } else {
                word.to_string()
            };
            let sep = if cur.is_empty() { 0 } else { text::width(sp.face, sp.px, " ") };
            let w = text::width(sp.face, sp.px, &piece);
            if cur_w + sep + w > width && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            let text = if cur.is_empty() { piece } else { format!(" {piece}") };
            let add_w = text::width(sp.face, sp.px, &text);
            /* merge into the previous span when style matches */
            match cur.last_mut() {
                Some(l)
                    if l.face == sp.face
                        && l.px == sp.px
                        && l.underline == sp.underline
                        && l.dy == sp.dy =>
                {
                    l.text.push_str(&text);
                }
                _ => cur.push(Span {
                    text,
                    face: sp.face,
                    px: sp.px,
                    underline: sp.underline,
                    dy: sp.dy,
                }),
            }
            cur_w += add_w;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// Hard character-wrap for mono content (code, tables).
fn hard_wrap_mono(s: &str, width: i32, px: f32) -> Vec<String> {
    let adv = text::advance(Face::Mono, px).max(1);
    let per = (width / adv).max(8) as usize;
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= per {
        return vec![s.to_string()];
    }
    chars.chunks(per).map(|c| c.iter().collect()).collect()
}
