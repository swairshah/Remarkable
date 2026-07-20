// CanvasView.swift — PencilKit wrapped in a gesture-owning container.
//
// This mirrors ../ipad/Collab's proven architecture. A raw touch observer
// lives on the CONTAINER above PencilKit, stays `.possible` for the entire
// touch sequence, and therefore cannot lose gesture arbitration to
// PencilKit (the old tap-only erase bug). Page paging is a separate
// finger-only recognizer: Apple Pencil can never move the page.

import PencilKit
import SwiftUI
import UIKit

/// The rubber's behavior — papier's three modes (toolbar.rs EraserMode).
enum EraserMode: String, CaseIterable {
    case object   // whole touched strokes vanish
    case pixel    // split only what the rubber covers
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

/// Collab's pager contract: stream the finger drag so the current and
/// neighboring sheets track it, then decide whether to commit on release.
enum PageDragPhase {
    case changed(CGFloat)
    case ended(translation: CGFloat, velocity: CGFloat)
    case cancelled
}

/// The document view's handle on the active page canvas, for undo/redo.
final class CanvasHub: ObservableObject {
    weak var activeCanvas: PKCanvasView?
}

/// Reports raw touches without ever recognizing. Remaining `.possible`
/// means PencilKit cannot starve it, cancel it, or require it to fail.
final class TouchObserverGesture: UIGestureRecognizer {
    var onTouch: ((CGPoint, UITouch.Phase, UITouch.TouchType) -> Void)?

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        report(touches, .began)
    }
    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent) {
        report(touches, .moved)
    }
    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
        report(touches, .ended)
        state = .failed
    }
    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) {
        report(touches, .cancelled)
        state = .failed
    }

    private func report(_ touches: Set<UITouch>, _ phase: UITouch.Phase) {
        guard let view, let touch = touches.first else { return }
        onTouch?(touch.location(in: view), phase, touch.type)
    }
}

struct CanvasView: UIViewRepresentable {
    let initialDrawing: PKDrawing
    let epoch: Int          // bumps only on genuine (re)load — see PageModel
    let tool: CanvasTool
    let eraserMode: EraserMode
    let fingerDraws: Bool
    /// false while a capture overlay (lasso / region erase) owns touches
    let interactionEnabled: Bool
    let isActive: Bool
    let hub: CanvasHub
    let onChanged: (PKDrawing) -> Void
    /// Apple Pencil side double-tap: toggle pencil ↔ last eraser mode.
    var onPencilDoubleTap: (() -> Void)?
    /// Finger-only horizontal page drag. Pencil never reaches this callback.
    var onPageDrag: ((PageDragPhase) -> Void)?
    /// Continuous eraser rub in DISPLAY coordinates; PageModel converts it.
    var onErase: ((CGPoint) -> Void)?

    func makeUIView(context: Context) -> UIView {
        let container = UIView()
        container.backgroundColor = .clear

        let canvas = PKCanvasView()
        canvas.overrideUserInterfaceStyle = .light
        canvas.backgroundColor = .clear
        canvas.isOpaque = false
        canvas.isScrollEnabled = false
        canvas.contentInsetAdjustmentBehavior = .never
        canvas.showsVerticalScrollIndicator = false
        canvas.showsHorizontalScrollIndicator = false
        canvas.bounces = false
        canvas.delegate = context.coordinator
        container.addSubview(canvas)
        canvas.frame = container.bounds
        canvas.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        context.coordinator.canvas = canvas

        let pencilInteraction = UIPencilInteraction()
        pencilInteraction.delegate = context.coordinator
        container.addInteraction(pencilInteraction)

        // Page swipes are DIRECT-FINGER ONLY. Pencil never reaches this.
        let pager = UIPanGestureRecognizer(target: context.coordinator,
                                           action: #selector(Coordinator.paged(_:)))
        pager.allowedTouchTypes = [NSNumber(value: UITouch.TouchType.direct.rawValue)]
        pager.maximumNumberOfTouches = 1

        // Raw observer gets pencil/finger points through PencilKit's subtree.
        let observer = TouchObserverGesture()
        observer.onTouch = { [weak coordinator = context.coordinator] point, phase, type in
            coordinator?.observedTouch(at: point, phase: phase, type: type)
        }

        for gesture in [pager, observer] as [UIGestureRecognizer] {
            gesture.cancelsTouchesInView = false
            gesture.delaysTouchesBegan = false
            gesture.delaysTouchesEnded = false
            gesture.delegate = context.coordinator
            container.addGestureRecognizer(gesture)
        }
        context.coordinator.pager = pager

        context.coordinator.programmatic = true
        canvas.drawing = initialDrawing
        context.coordinator.programmatic = false
        apply(to: canvas, coordinator: context.coordinator)
        if isActive { hub.activeCanvas = canvas }
        return container
    }

    func updateUIView(_ container: UIView, context: Context) {
        context.coordinator.parent = self
        guard let canvas = context.coordinator.canvas else { return }
        apply(to: canvas, coordinator: context.coordinator)
        if isActive { hub.activeCanvas = canvas }
        if context.coordinator.lastEpoch != epoch {
            context.coordinator.lastEpoch = epoch
            context.coordinator.programmatic = true
            canvas.drawing = initialDrawing
            context.coordinator.programmatic = false
        }
    }

    private func apply(to canvas: PKCanvasView, coordinator: Coordinator) {
        canvas.drawingPolicy = fingerDraws ? .anyInput : .pencilOnly
        canvas.isUserInteractionEnabled = interactionEnabled
        coordinator.pager?.isEnabled = isActive && interactionEnabled && !fingerDraws
        switch tool {
        case .pencil:
            canvas.tool = PencilBridge.pencilTool()
        case .eraser:
            switch eraserMode {
            case .object: canvas.tool = PKEraserTool(.vector)
            case .pixel: canvas.tool = PKEraserTool(.bitmap, width: 14)
            case .region: canvas.tool = PKEraserTool(.vector)
            }
        case .lasso:
            canvas.tool = PencilBridge.pencilTool()
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    final class Coordinator: NSObject, PKCanvasViewDelegate, UIGestureRecognizerDelegate,
                             UIPencilInteractionDelegate {
        var parent: CanvasView
        weak var canvas: PKCanvasView?
        var pager: UIPanGestureRecognizer?
        var programmatic = false
        var lastEpoch: Int
        private var lastRub: CGPoint?

        init(_ parent: CanvasView) {
            self.parent = parent
            self.lastEpoch = parent.epoch
        }

        func canvasViewDrawingDidChange(_ canvasView: PKCanvasView) {
            guard !programmatic else { return }
            parent.onChanged(canvasView.drawing)
        }

        // MARK: finger-only pager

        @objc func paged(_ gesture: UIPanGestureRecognizer) {
            switch gesture.state {
            case .began, .changed:
                parent.onPageDrag?(.changed(gesture.translation(in: gesture.view).x))
            case .ended:
                parent.onPageDrag?(.ended(
                    translation: gesture.translation(in: gesture.view).x,
                    velocity: gesture.velocity(in: gesture.view).x))
            case .cancelled, .failed:
                parent.onPageDrag?(.cancelled)
            default:
                break
            }
        }

        // MARK: continuous pi-ink eraser

        private func erase(at point: CGPoint) { parent.onErase?(point) }

        func observedTouch(at point: CGPoint, phase: UITouch.Phase, type: UITouch.TouchType) {
            guard parent.isActive, parent.tool == .eraser, parent.eraserMode != .region else {
                lastRub = nil
                return
            }
            // Pencil always erases. Finger erases only when finger drawing is enabled;
            // otherwise that finger remains exclusively available to the pager.
            guard type == .pencil || parent.fingerDraws else { return }
            switch phase {
            case .began:
                lastRub = point
                erase(at: point)
            case .moved:
                if let last = lastRub {
                    let distance = hypot(point.x - last.x, point.y - last.y)
                    let steps = max(1, Int(distance / 8))
                    for index in 1...steps {
                        let t = CGFloat(index) / CGFloat(steps)
                        erase(at: CGPoint(x: last.x + (point.x - last.x) * t,
                                          y: last.y + (point.y - last.y) * t))
                    }
                } else {
                    erase(at: point)
                }
                lastRub = point
            case .ended, .cancelled:
                lastRub = nil
            default:
                break
            }
        }

        // MARK: Apple Pencil side double-tap

        private func handlePencilDoubleTap() {
            guard parent.isActive else { return }
            parent.onPencilDoubleTap?()
        }

        func pencilInteractionDidTap(_ interaction: UIPencilInteraction) {
            handlePencilDoubleTap()
        }

        @available(iOS 17.5, *)
        func pencilInteraction(_ interaction: UIPencilInteraction,
                               didReceiveTap tap: UIPencilInteraction.Tap) {
            handlePencilDoubleTap()
        }

        // MARK: recognizer arbitration

        func gestureRecognizer(_ gesture: UIGestureRecognizer,
                               shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool {
            true
        }

        func gestureRecognizer(_ gesture: UIGestureRecognizer,
                               shouldBeRequiredToFailBy other: UIGestureRecognizer) -> Bool { false }

        func gestureRecognizer(_ gesture: UIGestureRecognizer,
                               shouldRequireFailureOf other: UIGestureRecognizer) -> Bool { false }

        func gestureRecognizerShouldBegin(_ gesture: UIGestureRecognizer) -> Bool {
            guard gesture === pager, let pan = gesture as? UIPanGestureRecognizer else { return true }
            guard parent.isActive, parent.interactionEnabled, !parent.fingerDraws else { return false }
            let velocity = pan.velocity(in: pan.view)
            return abs(velocity.x) > abs(velocity.y) * 1.4
        }
    }
}
