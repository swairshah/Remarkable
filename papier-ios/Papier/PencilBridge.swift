// PencilBridge.swift — papier ink <-> PencilKit.
//
// The user's strokes (top-level "strokes" in the page file) round-trip
// through PKCanvasView as PENCIL-textured PKStrokes, so everything on the
// iPad — old tablet ink and new marks alike — draws and erases like
// pencil. pi's ink ("patches") is papier's, not the user's: it renders
// into the page background in pi blue and is passed through verbatim on
// save. papier heals foreign stroke ids on load, so ids here just need to
// be unique within the file.

import PencilKit
import UIKit

enum PencilBridge {
    static let piBlue = UIColor(red: 0x24 / 255.0, green: 0x57 / 255.0, blue: 0xC5 / 255.0, alpha: 1)
    static let inkBlack = UIColor(white: 0.11, alpha: 1)

    /// Default drawing tool: pencil, like everything else the user asked for.
    static func pencilTool(width: CGFloat = 4.5) -> PKInkingTool {
        PKInkingTool(.pencil, color: inkBlack, width: width)
    }

    // MARK: papier -> PencilKit (display space)

    static func drawing(from page: InkPage, scale: CGFloat) -> PKDrawing {
        PKDrawing(strokes: page.strokes.map { stroke(from: $0, scale: scale) })
    }

    /// gray==1 is the invisible "iPad-pencil authored" watermark: it renders
    /// identically to black on the tablet/web, but tells us to bring the
    /// pencil texture back on reload (pressure reconstructed from radius).
    static let pencilGrayTag = 1

    private static func stroke(from s: InkStroke, scale: CGFloat) -> PKStroke {
        let isPencilTagged = s.gray == pencilGrayTag
        let color = (s.gray == 0 || isPencilTagged)
            ? inkBlack : UIColor(white: CGFloat(s.gray) / 255.0, alpha: 1)
        // Tablet/web-authored ink: monoline — a crisp geometric stroke,
        // matching how the tablet and the web viewer render it. (Pencil
        // texture through the pencil brush looks blotchy at the
        // reMarkable's stroke widths.) iPad-pencil-tagged ink: pencil brush,
        // so the pencily vibe survives the sync round trip.
        let ink = PKInk(isPencilTagged ? .pencil : .monoline, color: color)
        var points: [PKStrokePoint] = []
        points.reserveCapacity(max(s.points.count, 1))
        var t: TimeInterval = 0
        for p in s.points {
            let w: CGFloat
            let opacity: CGFloat
            if isPencilTagged {
                // EXACT inverse of the export map r = 2.4*(0.55 + 0.75*o):
                // recover the original per-point opacity — no re-shaping.
                opacity = CGFloat(max(0.15, min(1.0, (p.r / 2.4 - 0.55) / 0.75)))
                // The live pencil's tip footprint (measured: r ≈ 1.2 page px),
                // scaled to this display — weight comes from opacity+texture.
                w = max(1.3, 2 * 1.2 * scale)
            } else {
                opacity = 1
                w = max(1.5, CGFloat(p.r) * 2 * scale)
            }
            points.append(PKStrokePoint(location: CGPoint(x: p.x * scale, y: p.y * scale),
                                        timeOffset: t,
                                        size: CGSize(width: w, height: w),
                                        opacity: opacity, force: 1, azimuth: 0, altitude: .pi / 2))
            t += 0.008
        }
        if points.isEmpty {
            points.append(PKStrokePoint(location: .zero, timeOffset: 0,
                                        size: CGSize(width: 3, height: 3),
                                        opacity: 1, force: 1, azimuth: 0, altitude: .pi / 2))
        }
        return PKStroke(ink: ink, path: PKStrokePath(controlPoints: points, creationDate: Date()))
    }

    // MARK: PencilKit -> papier (page space)

    /// Convert the canvas drawing back to papier user strokes. Ids are
    /// assigned sequentially from `firstId`.
    static func inkStrokes(from drawing: PKDrawing, scale: CGFloat, firstId: UInt64) -> [InkStroke] {
        var id = firstId
        var out: [InkStroke] = []
        for stroke in drawing.strokes {
            // Pencil ink carries its visible weight in OPACITY (pressure);
            // its point size is just the constant tip footprint. Serializing
            // size would turn pencil marks into hairlines, so pressure maps
            // to the papier radius instead — calibrated to the reMarkable
            // pen's typical 1.7–3.1 page-px range. Other inks (monoline
            // imports) carry true geometry in size and round-trip directly.
            let isPencil = stroke.ink.inkType == .pencil
            // ~2.2 display-pt steps: dense enough for smooth e-ink redraw,
            // sparse enough to keep page files small.
            let pts = stroke.path.interpolatedPoints(by: .distance(2.2)).map { p -> InkPoint in
                let loc = p.location.applying(stroke.transform)
                let r: Double
                if isPencil {
                    let pressure = Double(max(0.15, min(1.0, p.opacity)))
                    r = 2.4 * (0.55 + 0.75 * pressure)   // page units: 1.7…3.1
                } else {
                    r = Double(max(0.8, min(6.0, (max(p.size.width, p.size.height) / 2) / scale)))
                }
                return InkPoint(x: Double(loc.x / scale),
                                y: Double(loc.y / scale),
                                r: r)
            }
            guard !pts.isEmpty else { continue }
            out.append(InkStroke(id: id,
                                 gray: isPencil ? pencilGrayTag : 0,
                                 points: Array(pts)))
            id += 1
        }
        return out
    }

    // MARK: pi patches -> background image

    /// Render pi's patches (blue strokes + typeset text) at display scale,
    /// exactly like the web viewer's overlay.
    static func patchesImage(page: InkPage, pageSize: CGSize, scale: CGFloat) -> UIImage? {
        guard !page.patches.isEmpty else { return nil }
        let size = CGSize(width: pageSize.width * scale, height: pageSize.height * scale)
        let fmt = UIGraphicsImageRendererFormat()
        fmt.opaque = false
        let renderer = UIGraphicsImageRenderer(size: size, format: fmt)
        return renderer.image { ctx in
            let cg = ctx.cgContext
            cg.setLineCap(.round)
            cg.setLineJoin(.round)
            for patch in page.patches {
                for s in patch.strokes {
                    cg.setStrokeColor(piBlue.cgColor)
                    guard let first = s.points.first else { continue }
                    if s.points.count == 1 {
                        let r = max(0.6, first.r * scale)
                        cg.setFillColor(piBlue.cgColor)
                        cg.fillEllipse(in: CGRect(x: first.x * scale - r, y: first.y * scale - r,
                                                  width: r * 2, height: r * 2))
                        continue
                    }
                    // stroke in segments so per-point width is honored
                    for i in 1..<s.points.count {
                        let a = s.points[i - 1], b = s.points[i]
                        cg.setLineWidth(max(1.0, (a.r + b.r) * scale))
                        cg.move(to: CGPoint(x: a.x * scale, y: a.y * scale))
                        cg.addLine(to: CGPoint(x: b.x * scale, y: b.y * scale))
                        cg.strokePath()
                    }
                }
                for t in patch.texts {
                    let fontSize = t.size * scale
                    let font = UIFont(name: "EBGaramond-Regular", size: fontSize)
                        ?? UIFont(name: "Georgia", size: fontSize)
                        ?? UIFont.systemFont(ofSize: fontSize)
                    let attr: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: piBlue]
                    // papier's TextRun y is the baseline; draw() wants the top.
                    let top = CGFloat(t.y) * scale - font.ascender
                    NSAttributedString(string: t.text, attributes: attr)
                        .draw(at: CGPoint(x: CGFloat(t.x) * scale, y: top))
                }
            }
        }
    }
}
