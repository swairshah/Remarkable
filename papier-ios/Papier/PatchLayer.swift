// PatchLayer.swift — pi's ink, rendered the iPad way: vector-crisp SwiftUI
// Canvas (no pre-rasterized bitmap), real Core Text EB Garamond for the
// typeset runs, and a draw-in animation when a fresh patch arrives from
// cloud pi — the tablet's "ghost hand", but at 120Hz.

import SwiftUI
import UIKit

struct PatchLayer: View {
    let patches: [InkPatch]
    let scale: CGFloat
    /// Patch ids that should animate in (fresh from pi), with their start.
    let animateIds: Set<UInt64>
    let animateStart: Date

    private static let piBlue = Color(red: 0x24 / 255.0, green: 0x57 / 255.0, blue: 0xC5 / 255.0)
    private static let drawInSeconds: Double = 1.4

    var body: some View {
        if animateIds.isEmpty {
            canvas(progress: 1)
        } else {
            TimelineView(.animation) { timeline in
                let elapsed = timeline.date.timeIntervalSince(animateStart)
                canvas(progress: min(1, max(0, elapsed / Self.drawInSeconds)))
            }
        }
    }

    private func canvas(progress: Double) -> some View {
        Canvas { ctx, _ in
            for patch in patches {
                let animated = animateIds.contains(patch.id)
                drawStrokes(patch, into: &ctx, reveal: animated ? progress : 1)
                if !animated || progress > 0.15 {
                    drawTexts(patch, into: &ctx,
                              opacity: animated ? min(1, (progress - 0.15) / 0.5) : 1)
                }
            }
        }
        .allowsHitTesting(false)
    }

    private func drawStrokes(_ patch: InkPatch, into ctx: inout GraphicsContext, reveal: Double) {
        guard reveal > 0 else { return }
        let total = max(patch.strokes.count, 1)
        for (i, s) in patch.strokes.enumerated() {
            guard s.points.count > 1 else {
                if let p = s.points.first, reveal >= Double(i + 1) / Double(total) {
                    let r = max(0.6, p.r * scale)
                    ctx.fill(Path(ellipseIn: CGRect(x: p.x * scale - r, y: p.y * scale - r,
                                                    width: r * 2, height: r * 2)),
                             with: .color(Self.piBlue))
                }
                continue
            }
            // strokes reveal sequentially: stroke i occupies the reveal
            // window [i/total, (i+1)/total]
            let lo = Double(i) / Double(total)
            let hi = Double(i + 1) / Double(total)
            let frac = reveal >= hi ? 1.0 : max(0, (reveal - lo) / (hi - lo))
            guard frac > 0 else { continue }

            var path = Path()
            path.move(to: CGPoint(x: s.points[0].x * scale, y: s.points[0].y * scale))
            for p in s.points.dropFirst() {
                path.addLine(to: CGPoint(x: p.x * scale, y: p.y * scale))
            }
            let drawn = frac >= 1 ? path : path.trimmedPath(from: 0, to: frac)
            let width = max(1.0, avgR(s) * 2 * scale)
            ctx.stroke(drawn, with: .color(Self.piBlue),
                       style: StrokeStyle(lineWidth: width, lineCap: .round, lineJoin: .round))
        }
    }

    private func drawTexts(_ patch: InkPatch, into ctx: inout GraphicsContext, opacity: Double) {
        guard opacity > 0 else { return }
        for t in patch.texts {
            let size = t.size * scale
            let uiFont = UIFont(name: "EBGaramond-Regular", size: size)
                ?? UIFont(name: "Georgia", size: size)
                ?? UIFont.systemFont(ofSize: size)
            let text = Text(t.text)
                .font(Font(uiFont as CTFont))
                .foregroundColor(Self.piBlue.opacity(opacity))
            // papier's TextRun y is the BASELINE; anchor at the top-left
            // using the font's real ascender.
            let top = CGPoint(x: t.x * scale, y: t.y * scale - uiFont.ascender)
            ctx.draw(ctx.resolve(text), at: top, anchor: .topLeading)
        }
    }

    private func avgR(_ s: InkStroke) -> CGFloat {
        guard !s.points.isEmpty else { return 1 }
        return s.points.reduce(0) { $0 + $1.r } / CGFloat(s.points.count)
    }
}
