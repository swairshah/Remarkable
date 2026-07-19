// PageModel.swift — one page's editing session: load ink (local pending
// beats stale mirror), expose it as a pencil PKDrawing + a pi-patches
// image, and debounce-save edits back through the cloud write path.

import PencilKit
import SwiftUI

enum SyncState: Equatable {
    case loading, clean, dirty, saving, saved, error(String)
}

@MainActor
final class PageModel: ObservableObject {
    let doc: PapierDoc
    let entry: SeqEntry
    private let store: LibraryStore

    @Published var patchesImage: UIImage?
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
    private var latestDrawing = PKDrawing()

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
        } else if (doc.ink ?? []).contains(key) {
            base = (try? await store.client.fetchInk(doc, key: key)) ?? InkPage()
        }
        initialDrawing = PencilBridge.drawing(from: base, scale: scale)
        latestDrawing = initialDrawing ?? PKDrawing()
        patchesImage = PencilBridge.patchesImage(page: base, pageSize: pageSize, scale: scale)
        drawingEpoch += 1
        sync = .clean
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
        patchesImage = PencilBridge.patchesImage(page: current, pageSize: pageSize, scale: scale)
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
            try? await Task.sleep(for: .seconds(2.5))
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
        } catch {
            sync = .error(error.localizedDescription)
        }
    }

    /// Best-effort flush when leaving the page / backgrounding the app.
    func flushNow() {
        saveGeneration += 1
        Task { await flush() }
    }
}
