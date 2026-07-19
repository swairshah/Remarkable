// PatchEraseOverlay.swift — a native touch surface only over pi ink.
//
// PencilKit owns Apple Pencil input and SwiftUI/gesture recognizers are not
// reliable when its eraser is active. This UIView sits above the canvas but
// returns true from point(inside:) ONLY over a pi patch's expanded bounds.
// A pencil/finger touch there erases that patch immediately; everywhere else
// falls through to PencilKit so user ink keeps normal object/pixel erasing.

import SwiftUI
import UIKit

struct PatchEraseOverlay: UIViewRepresentable {
    let patches: [InkPatch]
    let scale: CGFloat
    let onErase: (UInt64) -> Void

    func makeUIView(context: Context) -> PatchEraseTouchView {
        let view = PatchEraseTouchView()
        view.backgroundColor = .clear
        view.isOpaque = false
        view.isMultipleTouchEnabled = false
        update(view)
        return view
    }

    func updateUIView(_ view: PatchEraseTouchView, context: Context) {
        update(view)
    }

    private func update(_ view: PatchEraseTouchView) {
        view.regions = patches.compactMap { patch in
            guard let bounds = bounds(of: patch) else { return nil }
            // Broad enough to feel like the Pencil eraser nib, while still
            // allowing touches elsewhere to fall through to PencilKit.
            return (patch.id, bounds.insetBy(dx: -28, dy: -28))
        }
        view.onErase = onErase
    }

    private func bounds(of patch: InkPatch) -> CGRect? {
        var rect: CGRect?
        func include(_ r: CGRect) { rect = rect?.union(r) ?? r }

        for stroke in patch.strokes {
            for point in stroke.points {
                let r = max(1, CGFloat(point.r) * scale)
                include(CGRect(x: CGFloat(point.x) * scale - r,
                               y: CGFloat(point.y) * scale - r,
                               width: r * 2, height: r * 2))
            }
        }
        for text in patch.texts {
            let size = CGFloat(text.size) * scale
            let width = CGFloat(text.text.count) * size * 0.58
            include(CGRect(x: CGFloat(text.x) * scale,
                           y: CGFloat(text.y) * scale - size,
                           width: width, height: size * 1.3))
        }
        return rect
    }
}

final class PatchEraseTouchView: UIView {
    var regions: [(UInt64, CGRect)] = []
    var onErase: ((UInt64) -> Void)?
    private var erasedThisTouch: Set<UInt64> = []

    override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
        regions.contains { $0.1.contains(point) }
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        erasedThisTouch.removeAll()
        erase(at: touches.first?.location(in: self))
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        erase(at: touches.first?.location(in: self))
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        erasedThisTouch.removeAll()
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        erasedThisTouch.removeAll()
    }

    private func erase(at point: CGPoint?) {
        guard let point,
              let id = regions.first(where: { $0.1.contains(point) })?.0,
              !erasedThisTouch.contains(id) else { return }
        erasedThisTouch.insert(id)
        onErase?(id)
    }
}
