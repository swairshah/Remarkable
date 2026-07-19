// CanvasView.swift — the PKCanvasView wrapper. Transparent, sized to the
// page, pencil tool by default. Finger scrolls/flips pages; the Pencil
// draws (toggleable for finger drawing).

import PencilKit
import SwiftUI

enum CanvasTool: Equatable {
    case pencil
    case eraser
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
    let fingerDraws: Bool
    let isActive: Bool
    let hub: CanvasHub
    let onChanged: (PKDrawing) -> Void

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
        apply(to: canvas)
        return canvas
    }

    func updateUIView(_ canvas: PKCanvasView, context: Context) {
        apply(to: canvas)
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
        switch tool {
        case .pencil: canvas.tool = PencilBridge.pencilTool()
        case .eraser: canvas.tool = PKEraserTool(.vector)
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(onChanged: onChanged, lastEpoch: epoch)
    }

    final class Coordinator: NSObject, PKCanvasViewDelegate {
        let onChanged: (PKDrawing) -> Void
        var programmatic = false
        var lastEpoch: Int

        init(onChanged: @escaping (PKDrawing) -> Void, lastEpoch: Int) {
            self.onChanged = onChanged
            self.lastEpoch = lastEpoch
        }

        func canvasViewDrawingDidChange(_ canvasView: PKCanvasView) {
            guard !programmatic else { return }
            onChanged(canvasView.drawing)
        }
    }
}
