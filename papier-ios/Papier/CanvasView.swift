// CanvasView.swift — the PKCanvasView wrapper. Transparent, sized to the
// page, pencil tool by default. Finger scrolls/flips pages; the Pencil
// draws (toggleable for finger drawing).

import PencilKit
import SwiftUI

/// The rubber's behavior — papier's three modes (toolbar.rs EraserMode).
enum EraserMode: String, CaseIterable {
    case object   // whole strokes vanish at a touch
    case pixel    // only what the rubber covers is removed
    case region   // circle a region; on lift everything inside goes

    var next: EraserMode {
        switch self {
        case .object: return .pixel
        case .pixel: return .region
        case .region: return .object
        }
    }

    var symbol: String {
        switch self {
        case .object: return "eraser"
        case .pixel: return "eraser.line.dashed"
        case .region: return "circle.dashed"
        }
    }
}

enum CanvasTool: Equatable {
    case pencil
    case eraser
    case lasso
}

/// The document view's handle on whichever page canvas is active, for
/// undo/redo from the toolbar.
final class CanvasHub: ObservableObject {
    weak var activeCanvas: PKCanvasView?
}

struct CanvasView: UIViewRepresentable {
    let initialDrawing: PKDrawing
    let epoch: Int          // bumps only on genuine (re)load — see PageModel
    let tool: CanvasTool
    let eraserMode: EraserMode
    let fingerDraws: Bool
    /// false while a capture overlay (lasso / region erase) owns the touches
    let interactionEnabled: Bool
    let isActive: Bool
    let hub: CanvasHub
    let onChanged: (PKDrawing) -> Void
    /// Fired for taps of ANY input type (pencil included — the canvas
    /// swallows pencil touches, so SwiftUI gestures never see them).
    var onTap: ((CGPoint) -> Void)?

    func makeUIView(context: Context) -> PKCanvasView {
        let canvas = PKCanvasView()
        // Pin the canvas to LIGHT appearance: PencilKit dynamically inverts
        // ink colors for dark mode, which would wash near-black strokes out
        // to white on our always-paper-colored page.
        canvas.overrideUserInterfaceStyle = .light
        canvas.backgroundColor = .clear
        canvas.isOpaque = false
        canvas.isScrollEnabled = false
        canvas.contentInsetAdjustmentBehavior = .never
        canvas.delegate = context.coordinator
        context.coordinator.programmatic = true
        canvas.drawing = initialDrawing
        context.coordinator.programmatic = false
        let tap = UITapGestureRecognizer(target: context.coordinator,
                                         action: #selector(Coordinator.tapped(_:)))
        tap.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.direct.rawValue),
                                 NSNumber(value: UITouch.TouchType.pencil.rawValue)]
        tap.delegate = context.coordinator
        canvas.addGestureRecognizer(tap)
        apply(to: canvas)
        return canvas
    }

    func updateUIView(_ canvas: PKCanvasView, context: Context) {
        apply(to: canvas)
        context.coordinator.onTap = onTap
        if isActive { hub.activeCanvas = canvas }
        // Adopt a rebuilt drawing ONLY on a load/rescale epoch change —
        // never on ordinary SwiftUI refreshes, which would wipe user edits.
        if context.coordinator.lastEpoch != epoch {
            context.coordinator.lastEpoch = epoch
            context.coordinator.programmatic = true
            canvas.drawing = initialDrawing
            context.coordinator.programmatic = false
        }
    }

    private func apply(to canvas: PKCanvasView) {
        canvas.drawingPolicy = fingerDraws ? .anyInput : .pencilOnly
        canvas.isUserInteractionEnabled = interactionEnabled
        switch tool {
        case .pencil:
            canvas.tool = PencilBridge.pencilTool()
        case .eraser:
            switch eraserMode {
            case .object: canvas.tool = PKEraserTool(.vector)
            case .pixel: canvas.tool = PKEraserTool(.bitmap, width: 14)
            case .region: canvas.tool = PKEraserTool(.vector) // overlay captures instead
            }
        case .lasso:
            canvas.tool = PencilBridge.pencilTool() // overlay captures instead
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onChanged: onChanged, lastEpoch: epoch, onTap: onTap)
    }

    final class Coordinator: NSObject, PKCanvasViewDelegate, UIGestureRecognizerDelegate {
        let onChanged: (PKDrawing) -> Void
        var programmatic = false
        var lastEpoch: Int
        var onTap: ((CGPoint) -> Void)?

        init(onChanged: @escaping (PKDrawing) -> Void, lastEpoch: Int, onTap: ((CGPoint) -> Void)?) {
            self.onChanged = onChanged
            self.lastEpoch = lastEpoch
            self.onTap = onTap
        }

        func canvasViewDrawingDidChange(_ canvasView: PKCanvasView) {
            guard !programmatic else { return }
            onChanged(canvasView.drawing)
        }

        @objc func tapped(_ g: UITapGestureRecognizer) {
            onTap?(g.location(in: g.view))
        }

        // run alongside PencilKit's own recognizers, never instead of them
        func gestureRecognizer(_ g: UIGestureRecognizer,
                               shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool {
            true
        }
    }
}
