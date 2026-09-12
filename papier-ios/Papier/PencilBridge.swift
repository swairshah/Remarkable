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

    /// Highlighter bands ride the SAME page schema as ink: a pale `gray`
    /// and a fat radius (papier's src/highlight.rs). PencilKit's marker ink
    /// is translucent, so words under a band stay readable here exactly as
    /// the tablet's darkest-wins stamping keeps them readable there.
    static let highlightGray = 186
    static let highlightMinGray = 150
    static let highlightWidth: CGFloat = 26
    static let highlightColor = UIColor(white: CGFloat(highlightGray) / 255.0, alpha: 1)

    static func isHighlight(_ gray: Int) -> Bool { gray >= highlightMinGray }

    /// Default drawing tool: pencil, like everything else the user asked for.
    static func pencilTool(width: CGFloat = 4.5) -> PKInkingTool {
        PKInkingTool(.pencil, color: inkBlack, width: width)
    }

    static func highlighterTool(width: CGFloat = highlightWidth) -> PKInkingTool {
        PKInkingTool(.marker, color: highlightColor, width: width)
    }

    // MARK: papier -> PencilKit (display space)

    static func drawing(from page: InkPage, scale: CGFloat) -> PKDrawing {
        PKDrawing(strokes: page.strokes.map { stroke(from: $0, scale: scale) })
    }

    private static func stroke(from s: InkStroke, scale: CGFloat) -> PKStroke {
        let color = s.gray == 0 ? inkBlack : UIColor(white: CGFloat(s.gray) / 255.0, alpha: 1)
        // Monoline: a crisp geometric stroke, matching how the tablet and the
        // web viewer render this ink. (Pencil texture is for LIVE drawing
        // only — imported strokes through the pencil brush look blotchy at
        // the reMarkable's stroke widths.) Highlighter bands come back as
        // marker so they blend instead of painting over what they mark —
        // an opaque monoline band would hide the words.
        let ink = PKInk(isHighlight(s.gray) ? .marker : .monoline, color: color)
        var points: [PKStrokePoint] = []
        points.reserveCapacity(max(s.points.count, 1))
        var t: TimeInterval = 0
        for p in s.points {
            let w = max(1.5, CGFloat(p.r) * 2 * scale)
            points.append(PKStrokePoint(location: CGPoint(x: p.x * scale, y: p.y * scale),
                                        timeOffset: t,
                                        size: CGSize(width: w, height: w),
                                        opacity: 1, force: 1, azimuth: 0, altitude: .pi / 2))
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
            // Marker IS the highlighter: its footprint is the band's real
            // half-height, so it serializes from geometry, not pressure.
            let isMarker = stroke.ink.inkType == .marker
            // ~2.2 display-pt steps: dense enough for smooth e-ink redraw,
            // sparse enough to keep page files small.
            let pts = stroke.path.interpolatedPoints(by: .distance(2.2)).map { p -> InkPoint in
                let loc = p.location.applying(stroke.transform)
                let nib = (max(p.size.width, p.size.height) / 2) / scale
                let r: Double
                if isMarker {
                    r = Double(max(6.0, min(40.0, nib)))
                } else if isPencil {
                    let pressure = Double(max(0.15, min(1.0, p.opacity)))
                    r = 2.4 * (0.55 + 0.75 * pressure)   // page units: 1.7…3.1
                } else {
                    r = Double(max(0.8, min(6.0, nib)))
                }
                return InkPoint(x: Double(loc.x / scale),
                                y: Double(loc.y / scale),
                                r: r)
            }
            guard !pts.isEmpty else { continue }
            out.append(InkStroke(id: id, gray: isMarker ? highlightGray : 0, points: Array(pts)))
            id += 1
        }
        return out
    }

    // pi's patches render vector-crisp in PatchLayer.swift (SwiftUI Canvas
    // + Core Text) — no pre-rasterized bitmap.
}
