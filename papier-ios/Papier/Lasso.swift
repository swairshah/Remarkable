// Lasso.swift — the tablet's lasso + region-erase, iPad edition.
//
// CaptureView records a freehand path (pencil or finger) with live dashed
// feedback; the geometry helpers decide which user strokes / pi patches
// fall inside. Selection state drives the move/delete UI in DocumentView.

import PencilKit
import SwiftUI
import UIKit

// MARK: - geometry

enum InkGeometry {
    /// Ray-casting point-in-polygon (display space).
    static func contains(_ poly: [CGPoint], _ p: CGPoint) -> Bool {
        guard poly.count > 2 else { return false }
        var inside = false
        var j = poly.count - 1
        for i in 0..<poly.count {
            let a = poly[i], b = poly[j]
            if (a.y > p.y) != (b.y > p.y),
               p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
                inside.toggle()
            }
            j = i
        }
        return inside
    }

    /// Indices of drawing strokes with most of their points inside the polygon.
    static func strokesInside(_ drawing: PKDrawing, poly: [CGPoint]) -> [Int] {
        var out: [Int] = []
        for (i, stroke) in drawing.strokes.enumerated() {
            var inside = 0, total = 0
            for p in stroke.path.interpolatedPoints(by: .distance(8)) {
                total += 1
                if contains(poly, p.location.applying(stroke.transform)) { inside += 1 }
            }
            if total > 0 && Double(inside) / Double(total) > 0.55 { out.append(i) }
        }
        return out
    }

    /// Ids of patches whose strokes/texts fall mostly inside the polygon
    /// (poly in display space; patches in page space — pass the scale).
    static func patchesInside(_ patches: [InkPatch], poly: [CGPoint], scale: CGFloat) -> [UInt64] {
        var out: [UInt64] = []
        for patch in patches {
            var inside = 0, total = 0
            for s in patch.strokes {
                for p in s.points {
                    total += 1
                    if contains(poly, CGPoint(x: p.x * scale, y: p.y * scale)) { inside += 1 }
                }
            }
            for t in patch.texts {
                total += 1
                if contains(poly, CGPoint(x: t.x * scale, y: (t.y - t.size / 2) * scale)) { inside += 1 }
            }
            if total > 0 && Double(inside) / Double(total) > 0.55 { out.append(patch.id) }
        }
        return out
    }

    static func bounds(of points: [CGPoint]) -> CGRect {
        guard let first = points.first else { return .zero }
        var r = CGRect(origin: first, size: .zero)
        for p in points.dropFirst() {
            r = r.union(CGRect(origin: p, size: .zero))
        }
        return r
    }
}

// MARK: - selection state

struct InkSelection {
    var strokeIndices: [Int]      // into the canvas drawing
    var patchIds: [UInt64]        // pi patches
    var bbox: CGRect              // display space
    var offset: CGSize = .zero    // live drag offset
    var isEmpty: Bool { strokeIndices.isEmpty && patchIds.isEmpty }
}

// MARK: - freehand capture overlay (lasso / region erase)

struct CaptureView: UIViewRepresentable {
    let dashed: Bool
    let onComplete: ([CGPoint]) -> Void

    func makeUIView(context: Context) -> CaptureUIView {
        let v = CaptureUIView()
        v.backgroundColor = .clear
        v.dashed = dashed
        v.onComplete = onComplete
        return v
    }

    func updateUIView(_ v: CaptureUIView, context: Context) {
        v.onComplete = onComplete
    }
}

final class CaptureUIView: UIView {
    var onComplete: (([CGPoint]) -> Void)?
    var dashed = true
    private var points: [CGPoint] = []
    private let shape = CAShapeLayer()

    override init(frame: CGRect) {
        super.init(frame: frame)
        shape.fillColor = UIColor.systemBlue.withAlphaComponent(0.06).cgColor
        shape.strokeColor = UIColor.systemBlue.withAlphaComponent(0.8).cgColor
        shape.lineWidth = 2
        shape.lineDashPattern = [6, 4]
        layer.addSublayer(shape)
    }

    required init?(coder: NSCoder) { fatalError() }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        points = touches.first.map { [$0.location(in: self)] } ?? []
        render()
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        guard let t = touches.first else { return }
        points.append(t.location(in: self))
        render()
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        finish()
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        points = []
        render()
    }

    private func finish() {
        let captured = points
        points = []
        render()
        if captured.count > 8 { onComplete?(captured) }
    }

    private func render() {
        let path = UIBezierPath()
        if let first = points.first {
            path.move(to: first)
            for p in points.dropFirst() { path.addLine(to: p) }
            if points.count > 2 { path.close() }
        }
        shape.path = path.cgPath
        shape.lineDashPattern = dashed ? [6, 4] : nil
    }
}
