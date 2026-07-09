# hershey-math — MATH glyph table generator

Generates the `MATH` table + `math_glyph()` at the bottom of
`src/hershey_data.rs` (also copied verbatim into reader's) from
`mathlow.jhf` (public-domain Hershey math symbols, from
github.com/kamalmostafa/hershey-fonts) plus three hand-drawn glyphs
(∀ ∅ ≈) synthesized in gen_math.py.

    python3 gen_math.py   # writes math_table.rs + math_check.png

Eyeball math_check.png, then replace the MATH section of both apps'
hershey_data.rs with math_table.rs. To add a symbol: find its index by
contact-sheet (see gen_math.py's GLYPHS list for the known ones), add a
(codepoint, comment, from_jhf(i)) row, regenerate.
