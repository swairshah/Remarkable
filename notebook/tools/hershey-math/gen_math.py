"""Generate the MATH Hershey table (Rust) from mathlow.jhf + hand glyphs.

Emits math_table.rs (const MATH + math_glyph()) and math_check.png.
"""
import math

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

from jhf import parse_jhf, polylines

src = parse_jhf("mathlow.jhf")

PEN_UP = None


def from_jhf(i, adv_override=None, left_override=None):
    _, l, r, pts = src[i]
    return ((adv_override or (r - l)), (left_override or l), list(pts))


# --- hand-authored glyphs (same units: y down, baseline 9, cap top -12) ---

def forall():
    pts = [(-7, -12), (0, 9), PEN_UP, (7, -12), (0, 9), PEN_UP, (-4, -2), (4, -2)]
    return (18, -9, pts)


def emptyset():
    pts = []
    for k in range(13):  # closed circle, r=7, center (0,-2)
        a = 2 * math.pi * k / 12
        pts.append((round(7 * math.sin(a)), round(-2 - 7 * math.cos(a))))
    pts.append(PEN_UP)
    pts += [(-6, 6), (6, -10)]
    return (20, -10, pts)


def approx():  # two single tilde waves stacked (the full ~ is double-stroked)
    _, _, pts = from_jhf(95)
    wave = polylines(pts)[0]
    lo = [(x, y - 3) for (x, y) in wave]
    hi = [(x, y + 3) for (x, y) in wave]
    return (24, -12, lo + [PEN_UP] + hi)


# (codepoint, comment, (adv, left, pts))
GLYPHS = [
    (0x00B0, "degree", from_jhf(64)),
    (0x00B1, "plus-minus", from_jhf(1)),
    (0x00B7, "middle dot / cdot", from_jhf(4, adv_override=10, left_override=-5)),
    (0x00D7, "times", from_jhf(3)),
    (0x00F7, "divide", from_jhf(88)),
    (0x2190, "left arrow", from_jhf(75)),
    (0x2191, "up arrow", from_jhf(74)),
    (0x2192, "right arrow", from_jhf(73)),
    (0x2193, "down arrow", from_jhf(76)),
    (0x2200, "for all", forall()),
    (0x2202, "partial", from_jhf(77)),
    (0x2203, "exists", from_jhf(86)),
    (0x2205, "empty set", emptyset()),
    (0x2207, "nabla", from_jhf(78)),
    (0x2208, "element of", from_jhf(72)),
    (0x220F, "n-ary product", from_jhf(26)),
    (0x2211, "n-ary summation", from_jhf(27)),
    (0x2213, "minus-plus", from_jhf(2)),
    (0x221A, "square root", from_jhf(66)),
    (0x221D, "proportional to", from_jhf(62)),
    (0x221E, "infinity", from_jhf(63)),
    (0x2220, "angle", from_jhf(92)),
    (0x2229, "intersection", from_jhf(71)),
    (0x222A, "union", from_jhf(69)),
    (0x222B, "integral", from_jhf(80)),
    (0x223C, "tilde operator", from_jhf(95)),
    (0x2234, "therefore", from_jhf(94)),
    (0x2248, "almost equal", approx()),
    (0x2260, "not equal", from_jhf(31)),
    (0x2261, "identical to", from_jhf(32)),
    (0x2264, "less-or-equal", from_jhf(6)),
    (0x2265, "greater-or-equal", from_jhf(7)),
    (0x2282, "subset of", from_jhf(68)),
    (0x2283, "superset of", from_jhf(70)),
    (0x22A5, "perpendicular", from_jhf(90)),
]
ALIASES = {0x22C5: 0x00B7}  # dot operator -> middle dot glyph

# --- emit Rust ---
lines = []
lines.append("// MATH: common math symbols (mathlow.jhf + a few hand-drawn:")
lines.append("// forall / empty set / almost-equal), looked up per char via")
lines.append("// math_glyph() — same shapes for every face.")
lines.append(f"pub(crate) const MATH: [Glyph; {len(GLYPHS)}] = [")
for cp, comment, (adv, left, pts) in GLYPHS:
    rp = ", ".join("(-64,-64)" if p is None else f"({p[0]},{p[1]})" for p in pts)
    lines.append(
        f"    Glyph {{ adv: {adv}, left: {left}, pts: &[{rp}] }}, "
        f"// '{chr(cp)}' U+{cp:04X} {comment}"
    )
lines.append("];")
lines.append("")
lines.append("/// Math-symbol lookup (any face); None = not covered.")
lines.append("pub(crate) fn math_glyph(c: char) -> Option<&'static Glyph> {")
lines.append("    let i = match c as u32 {")
for idx, (cp, comment, _) in enumerate(GLYPHS):
    al = [a for a, t in ALIASES.items() if t == cp]
    pat = " | ".join(f"0x{x:04X}" for x in [cp] + al)
    lines.append(f"        {pat} => {idx}, // '{chr(cp)}' {comment}")
lines.append("        _ => return None,")
lines.append("    };")
lines.append("    Some(&MATH[i])")
lines.append("}")

with open("math_table.rs", "w") as f:
    f.write("\n".join(lines) + "\n")
print(f"wrote math_table.rs ({len(GLYPHS)} glyphs)")

# --- verification sheet ---
cols = 9
rows = (len(GLYPHS) + cols - 1) // cols
fig, axes = plt.subplots(rows, cols, figsize=(cols * 1.6, rows * 1.9))
for ax in axes.flat:
    ax.axis("off")
for idx, (cp, comment, (adv, left, pts)) in enumerate(GLYPHS):
    ax = axes.flat[idx]
    for line in polylines(pts):
        ax.plot([p[0] for p in line], [p[1] for p in line], "k-", lw=1.2)
    ax.axhline(9, color="0.8", lw=0.5)   # baseline
    ax.axhline(-12, color="0.9", lw=0.5)  # cap top
    ax.set_xlim(-20, 20)
    ax.set_ylim(20, -20)
    ax.set_aspect("equal")
    ax.set_title(f"{chr(cp)}  U+{cp:04X}", fontsize=9)
fig.tight_layout()
fig.savefig("math_check.png", dpi=90)
print("wrote math_check.png")
