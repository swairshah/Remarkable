// PageModel.swift — one page's editing session: load ink (local pending
// beats stale mirror), expose it as a pencil PKDrawing + a pi-patches
// image, and debounce-save edits back through the cloud write path.

import PencilKit
import SwiftUI
import UIKit

enum SyncState: Equatable {
    case loading, clean, dirty, saving, saved, error(String)
}

@MainActor
final class PageModel: ObservableObject {
    let doc: PapierDoc
    let entry: SeqEntry
    private let store: LibraryStore

    @Published var patches: [InkPatch] = []
    @Published var animateIds: Set<UInt64> = []
    @Published var animateStart = Date.distantPast
    @Published var initialDrawing: PKDrawing?
    /// Bumped ONLY when initialDrawing is genuinely (re)built (load/rescale):
    /// the canvas adopts a pushed drawing solely on epoch change, so user
    /// edits are never clobbered by unrelated SwiftUI refreshes.
    @Published var drawingEpoch = 0
    @Published var sync: SyncState = .loading

    private(set) var scale: CGFloat = 0
    private var base = InkPage()
    private var loaded = false
    private var saveGeneration = 0
    private var patchSyncGeneration: [UInt64: Int] = [:]
    private var latestDrawing = PKDrawing()

    /// Called after a successful cloud save — DocumentView reports the
    /// pause to remote pi.
    var onSaved: (() -> Void)?

    init(doc: PapierDoc, entry: SeqEntry, store: LibraryStore) {
        self.doc = doc
        self.entry = entry
        self.store = store
    }

    var pageSize: CGSize { CGSize(width: doc.pageW, height: doc.pageH) }

    func load(displayWidth: CGFloat) async {
        guard !loaded, displayWidth > 1 else { return }
        loaded = true
        scale = displayWidth / pageSize.width
        let key = entry.inkKey

        if let pending = store.pendingInk(docId: doc.id, key: key, currentVersion: doc.version) {
            base = pending
            // even with local pending strokes, the server may hold newer
            // pi patches — they are server-authoritative
            if let remote = try? await store.client.fetchInk(doc, key: key) {
                base.patches = remote.patches
                base.nextPatch = max(base.nextPatch, remote.nextPatch)
            }
        } else {
            base = (try? await store.client.fetchInk(doc, key: key)).flatMap { $0 } ?? InkPage()
        }
        initialDrawing = PencilBridge.drawing(from: base, scale: scale)
        latestDrawing = initialDrawing ?? PKDrawing()
        patches = base.patches
        drawingEpoch += 1
        sync = .clean
    }

    /// Cloud pi drew/erased on this page: adopt the server's patches
    /// (user strokes stay local truth) and animate the new ones in.
    func refreshPatches() async {
        guard loaded else { return }
        guard let remote = try? await store.client.fetchInk(doc, key: entry.inkKey) else { return }
        let known = Set(base.patches.map(\.id))
        base.patches = remote.patches
        base.nextPatch = max(base.nextPatch, remote.nextPatch)
        let fresh = Set(remote.patches.map(\.id)).subtracting(known)
        patches = base.patches
        if !fresh.isEmpty {
            animateIds = fresh
            animateStart = Date()
        }
    }

    /// Rotation / resize: rebuild the display-space drawing from page space.
    func rescale(displayWidth: CGFloat) {
        guard loaded, displayWidth > 1 else { return }
        let newScale = displayWidth / pageSize.width
        guard abs(newScale - scale) > 0.001 else { return }
        let current = exportPage(from: latestDrawing)
        scale = newScale
        base = current
        initialDrawing = PencilBridge.drawing(from: current, scale: scale)
        latestDrawing = initialDrawing ?? PKDrawing()
        patches = base.patches
        drawingEpoch += 1
    }

    // MARK: - saving

    func drawingChanged(_ drawing: PKDrawing) {
        guard loaded, sync != .loading else { return }
        latestDrawing = drawing
        sync = .dirty
        saveGeneration += 1
        let gen = saveGeneration
        Task { [weak self] in
            // Match the reMarkable: 2.8s of genuine pen inactivity before
            // auto-pi sees the page. Short between-word pauses are writing,
            // not invitations. Explicit nudges remain immediate.
            try? await Task.sleep(for: .seconds(2.8))
            guard let self, self.saveGeneration == gen else { return }
            await self.flush()
        }
    }

    private func exportPage(from drawing: PKDrawing) -> InkPage {
        // patch strokes keep their ids; user strokes renumber above them
        let patchMax = base.patches.flatMap(\.strokes).map(\.id).max() ?? 0
        let firstId = patchMax + 1
        var page = base
        page.strokes = PencilBridge.inkStrokes(from: drawing, scale: scale, firstId: firstId)
        page.nextStroke = (page.strokes.map(\.id).max() ?? patchMax) + 1
        return page
    }

    func flush() async {
        guard sync == .dirty else { return }
        sync = .saving
        let page = exportPage(from: latestDrawing)
        do {
            try await store.client.postInk(docId: doc.id, file: entry.inkKey + ".json", page: page)
            store.rememberPending(docId: doc.id, key: entry.inkKey, page: page, baseVersion: doc.version)
            base = page
            sync = .saved
            onSaved?()
        } catch {
            sync = .error(error.localizedDescription)
        }
    }

    /// Best-effort flush when leaving the page / backgrounding the app.
    func flushNow() {
        saveGeneration += 1
        Task { await flush() }
    }

    // MARK: - lasso / region edits

    /// Programmatic canvas replacement (lasso move / region erase of user
    /// strokes): push the drawing, mark dirty, schedule the save.
    func setDrawing(_ drawing: PKDrawing) {
        initialDrawing = drawing
        latestDrawing = drawing
        drawingEpoch += 1
        drawingChanged(drawing)
    }

    var currentDrawing: PKDrawing { latestDrawing }

    /// Erase a set of pi patches (local + server).
    func erasePatches(ids: [UInt64]) {
        guard !ids.isEmpty else { return }
        base.patches.removeAll { ids.contains($0.id) }
        patches = base.patches
        for id in ids {
            Task {
                try? await store.client.erasePatch(docId: doc.id, file: entry.inkKey + ".json", patchId: id)
            }
        }
    }

    /// Move a set of pi patches by a display-space delta (local + server).
    func movePatches(ids: [UInt64], by delta: CGSize) {
        guard !ids.isEmpty, scale > 0 else { return }
        let dx = delta.width / scale, dy = delta.height / scale
        for i in base.patches.indices where ids.contains(base.patches[i].id) {
            for s in base.patches[i].strokes.indices {
                for p in base.patches[i].strokes[s].points.indices {
                    base.patches[i].strokes[s].points[p].x += dx
                    base.patches[i].strokes[s].points[p].y += dy
                }
            }
            for t in base.patches[i].texts.indices {
                base.patches[i].texts[t].x += dx
                base.patches[i].texts[t].y += dy
            }
        }
        patches = base.patches
        for id in ids {
            Task {
                try? await store.client.movePatch(docId: doc.id, file: entry.inkKey + ".json",
                                                  patchId: id, dx: dx, dy: dy)
            }
        }
    }

    // MARK: - continuous erasing of pi ink (ported from ../ipad/Collab)

    /// Rub in DISPLAY coordinates. Object mode removes only touched Hershey
    /// strokes (letters/marks), pixel mode splits strokes around the nib, and
    /// typeset Garamond erases glyph-by-glyph. It never deletes a whole sentence.
    func erasePiInk(atDisplayPoint displayPoint: CGPoint, mode: EraserMode) {
        guard loaded, scale > 0, mode != .region else { return }
        let point = CGPoint(x: displayPoint.x / scale, y: displayPoint.y / scale)
        let radius = 26 / scale
        var changedIds: Set<UInt64> = []

        for index in base.patches.indices {
            var patch = base.patches[index]
            var changed = false

            if mode == .pixel {
                var survivors: [InkStroke] = []
                for stroke in patch.strokes {
                    let pieces = split(stroke: stroke, around: point, radius: radius)
                    if pieces.count != 1 || pieces.first?.points.count != stroke.points.count {
                        changed = true
                    }
                    survivors.append(contentsOf: pieces)
                }
                patch.strokes = survivors
            } else {
                let before = patch.strokes.count
                patch.strokes.removeAll { strokeHit($0, point: point, radius: radius) }
                changed = patch.strokes.count != before
            }

            var textSurvivors: [InkTextRun] = []
            for text in patch.texts {
                if let pieces = eraseGlyphs(from: text, at: point, radius: radius) {
                    textSurvivors.append(contentsOf: pieces)
                    changed = true
                } else {
                    textSurvivors.append(text)
                }
            }
            patch.texts = textSurvivors

            if changed {
                base.patches[index] = patch
                changedIds.insert(patch.id)
                animateIds.remove(patch.id)
            }
        }

        guard !changedIds.isEmpty else { return }
        base.patches.removeAll { $0.strokes.isEmpty && $0.texts.isEmpty }
        patches = base.patches
        for id in changedIds { schedulePatchSync(id: id) }
    }

    private func split(stroke: InkStroke, around point: CGPoint,
                       radius: CGFloat) -> [InkStroke] {
        var groups: [[InkPoint]] = []
        var current: [InkPoint] = []
        for p in stroke.points {
            let erased = hypot(CGFloat(p.x) - point.x, CGFloat(p.y) - point.y)
                <= radius + CGFloat(p.r)
            if erased {
                if !current.isEmpty { groups.append(current); current = [] }
            } else {
                current.append(p)
            }
        }
        if !current.isEmpty { groups.append(current) }
        guard !(groups.count == 1 && groups[0].count == stroke.points.count) else { return [stroke] }

        var pieces: [InkStroke] = []
        for (index, points) in groups.enumerated() where !points.isEmpty {
            let id: UInt64
            if index == 0 { id = stroke.id }
            else { id = base.nextStroke; base.nextStroke += 1 }
            pieces.append(InkStroke(id: id, gray: stroke.gray, points: points))
        }
        return pieces
    }

    private func strokeHit(_ stroke: InkStroke, point: CGPoint, radius: CGFloat) -> Bool {
        guard let first = stroke.points.first else { return false }
        let firstPoint = CGPoint(x: first.x, y: first.y)
        if stroke.points.count == 1 {
            return hypot(firstPoint.x - point.x, firstPoint.y - point.y) <= radius + first.r
        }
        for pair in zip(stroke.points, stroke.points.dropFirst()) {
            let a = CGPoint(x: pair.0.x, y: pair.0.y)
            let b = CGPoint(x: pair.1.x, y: pair.1.y)
            let dx = b.x - a.x, dy = b.y - a.y
            let length2 = dx * dx + dy * dy
            let t = length2 == 0 ? 0
                : max(0, min(1, ((point.x - a.x) * dx + (point.y - a.y) * dy) / length2))
            let closest = CGPoint(x: a.x + t * dx, y: a.y + t * dy)
            let inkRadius = max(CGFloat(pair.0.r), CGFloat(pair.1.r))
            if hypot(point.x - closest.x, point.y - closest.y) <= radius + inkRadius { return true }
        }
        return false
    }

    /// Erase rendered Garamond one grapheme at a time and split the run around
    /// the gap. Prefix widths come from the same bundled font PatchLayer uses.
    private func eraseGlyphs(from text: InkTextRun, at point: CGPoint,
                             radius: CGFloat) -> [InkTextRun]? {
        guard point.y >= text.y - text.size - radius,
              point.y <= text.y + text.size * 0.35 + radius else { return nil }
        let atoms = text.text.map(String.init)
        guard !atoms.isEmpty else { return nil }
        let font = UIFont(name: "EBGaramond-Regular", size: text.size)
            ?? UIFont(name: "Georgia", size: text.size)
            ?? UIFont.systemFont(ofSize: text.size)
        var offsets: [CGFloat] = [0]
        var prefix = ""
        for atom in atoms {
            prefix += atom
            offsets.append((prefix as NSString).size(withAttributes: [.font: font]).width)
        }
        guard point.x >= text.x - radius,
              point.x <= text.x + offsets.last! + radius else { return nil }

        var keep = [Bool](repeating: true, count: atoms.count)
        var hit = false
        for index in atoms.indices {
            let x0 = text.x + offsets[index]
            let x1 = text.x + offsets[index + 1]
            if point.x + radius >= x0 && point.x - radius <= x1 {
                keep[index] = false
                hit = true
            }
        }
        guard hit else { return nil }

        var output: [InkTextRun] = []
        var index = 0
        while index < atoms.count {
            guard keep[index] else { index += 1; continue }
            let start = index
            while index < atoms.count, keep[index] { index += 1 }
            let value = atoms[start..<index].joined()
            if !value.trimmingCharacters(in: .whitespaces).isEmpty {
                output.append(InkTextRun(x: text.x + offsets[start], y: text.y,
                                         size: text.size, gray: text.gray, text: value))
            }
        }
        return output
    }

    private func schedulePatchSync(id: UInt64) {
        patchSyncGeneration[id, default: 0] += 1
        let generation = patchSyncGeneration[id]!
        Task { [weak self] in
            try? await Task.sleep(for: .seconds(0.35))
            guard let self, self.patchSyncGeneration[id] == generation else { return }
            do {
                if let patch = self.base.patches.first(where: { $0.id == id }) {
                    try await self.store.client.replacePatch(
                        docId: self.doc.id, file: self.entry.inkKey + ".json",
                        patch: patch, nextStroke: self.base.nextStroke)
                } else {
                    try await self.store.client.erasePatch(
                        docId: self.doc.id, file: self.entry.inkKey + ".json", patchId: id)
                }
            } catch {
                self.sync = .error("pi erase sync: \(error.localizedDescription)")
            }
        }
    }
}
