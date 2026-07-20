// DocumentView.swift — a document open full-screen: swipe (or arrow) page
// navigation over the doc's seq, a papier-style floating right-edge tool
// rail (pencil / eraser / finger / undo / redo), page counter, add-page
// for notebooks. Books show the pre-rendered raster under the ink.

import PencilKit
import SwiftUI

struct DocumentView: View {
    let doc: PapierDoc

    @EnvironmentObject private var store: LibraryStore
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.dismiss) private var dismiss

    @State private var seq: [SeqEntry]
    @State private var index: Int
    @State private var tool: CanvasTool = .pencil
    @State private var eraserMode: EraserMode = .object
    @State private var fingerDraws = false
    @State private var askGoTo = false
    @State private var goToText = ""
    @State private var dragX: CGFloat = 0
    @State private var flipAnimating = false
    @State private var flipTarget: Int?
    @State private var pageWidth: CGFloat = UIScreen.main.bounds.width
    private let pageGap: CGFloat = 24
    // Synchronous identity cache: the page onscreen and pi's event handler
    // MUST share the exact same PageModel. An async @State insertion here
    // used to create twins; pi refreshed the hidden twin and nothing appeared.
    @StateObject private var models = PageModelCache()
    @StateObject private var hub = CanvasHub()
    @StateObject private var pi: PiSession

    init(doc: PapierDoc) {
        self.doc = doc
        _seq = State(initialValue: doc.seq.isEmpty ? [.note(1)] : doc.seq)
        let saved = UserDefaults.standard.integer(forKey: "pos-\(doc.id)")
        _index = State(initialValue: min(max(saved, 0), max((doc.seq.count) - 1, 0)))
        _pi = StateObject(wrappedValue: PiSession(docId: doc.id, serverRoot: ""))
    }

    private func model(for entry: SeqEntry) -> PageModel {
        let m = models.model(doc: doc, entry: entry, store: store)
        m.onSaved = { [weak pi] in
            // the debounced save just landed — that IS the writing pause
            if let i = seq.firstIndex(of: entry) { pi?.pause(page: i + 1) }
        }
        return m
    }

    private struct DeckSlot: Hashable {
        let idx: Int
        let slot: Int
    }

    /// prev/current/next stay PERMANENTLY mounted (like the old TabView
    /// pager). Mounting the incoming sheet mid-gesture is what flashed:
    /// it appeared blank, then its raster/ink popped in while sliding.
    private var deckSlots: [DeckSlot] {
        var slots: [DeckSlot] = []
        if let target = flipTarget, target != index {
            slots.append(DeckSlot(idx: target, slot: target > index ? 1 : -1))
        } else {
            if index > 0 { slots.append(DeckSlot(idx: index - 1, slot: -1)) }
            if index + 1 < seq.count { slots.append(DeckSlot(idx: index + 1, slot: 1)) }
        }
        if seq.indices.contains(index) { slots.append(DeckSlot(idx: index, slot: 0)) }
        return slots
    }

    @ViewBuilder
    private func pageDeck(in size: CGSize) -> some View {
        let distance = size.width + pageGap
        ZStack {
            ForEach(deckSlots, id: \.idx) { slot in
                pageScreen(at: slot.idx, active: slot.idx == index)
                    .offset(x: CGFloat(slot.slot) * distance + dragX)
            }
        }
        .clipped()
        .onAppear { pageWidth = size.width }
        .onChange(of: size.width) { _, width in pageWidth = width }
    }

    private func pageScreen(at pageIndex: Int, active: Bool) -> some View {
        let entry = seq[pageIndex]
        return PageScreen(doc: doc,
                          entry: entry,
                          model: model(for: entry),
                          near: true,
                          active: active,
                          tool: tool,
                          eraserMode: eraserMode,
                          fingerDraws: fingerDraws,
                          hub: hub,
                          onPencilDoubleTap: togglePencilEraser,
                          onPageDrag: active ? handlePageDrag : { _ in })
            // ONE id per page regardless of role: at flip commit the
            // neighbor sheet BECOMES the active sheet in place. Distinct
            // neighbor/active ids rebuilt the page after landing — raster
            // refetch + canvas recreation = the visible flash.
            .id(entry.inkKey)
    }

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color(uiColor: .systemGray6).ignoresSafeArea()

                // Collab's live two-sheet pager: the active sheet tracks the
                // finger and the neighbor follows beside it. Pencil never
                // reaches this finger-only drag state machine.
                pageDeck(in: geo.size)
                    .ignoresSafeArea(edges: .bottom)

                toolRail
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .trailing)
                    .padding(.trailing, 10)

                if let toast = pi.toast {
                    Text(toast)
                        .font(.callout)
                        .padding(.horizontal, 16).padding(.vertical, 10)
                        .background(.regularMaterial, in: Capsule())
                        .shadow(color: .black.opacity(0.15), radius: 8, y: 2)
                        .padding(.top, 8)
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                        .transition(.move(edge: .top).combined(with: .opacity))
                }
            }
            .animation(.spring(duration: 0.35), value: pi.toast)
        }
        .navigationTitle(doc.meta.title)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button { dismiss() } label: { Image(systemName: "chevron.left") }
                    .accessibilityLabel("Back to library")
            }
            ToolbarItemGroup(placement: .topBarTrailing) {
                syncBadge
                Text("\(index + 1) / \(seq.count)")
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(.secondary)
                Button { addPage() } label: { Image(systemName: "plus.square.on.square") }
                    .help("Insert a note page after this one")
            }
        }
        .alert("Go to page", isPresented: $askGoTo) {
            TextField("1–\(seq.count)", text: $goToText)
                .keyboardType(.numberPad)
            Button("Go") {
                if let n = Int(goToText), n >= 1, n <= seq.count {
                    animatePage(to: n - 1)
                }
                goToText = ""
            }
            Button("Cancel", role: .cancel) { goToText = "" }
        }
        .onChange(of: index) { old, _ in
            UserDefaults.standard.set(index, forKey: "pos-\(doc.id)")
            if old < seq.count { models[seq[old].inkKey]?.flushNow() }
            pi.reportPage(index + 1)
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { flushAll() }
        }
        .onDisappear { flushAll(); pi.close(); store.startPolling() }
        .onAppear {
            store.stopPolling()
            wirePi()
            pi.open()
            pi.reportPage(index + 1)
        }
    }

    private func wirePi() {
        pi.serverRoot = store.serverRoot.trimmingCharacters(in: .whitespaces)
        pi.onPatch = { page in
            guard page >= 1, page <= seq.count else { return }
            let entry = seq[page - 1]
            // model(...) is identity-stable even if the page was not
            // previously materialized, so a patch event cannot be lost.
            let pageModel = models.model(doc: doc, entry: entry, store: store)
            Task { await pageModel.refreshPatches() }
        }
        pi.onTurnEnd = {
            // Belt-and-suspenders: a patch event and a turn-end both pull
            // the active page. This makes old server/event cursors harmless.
            guard index >= 0, index < seq.count else { return }
            let pageModel = models.model(doc: doc, entry: seq[index], store: store)
            Task { await pageModel.refreshPatches() }
        }
        pi.onGoto = { page in
            guard page >= 1, page <= seq.count else { return }
            animatePage(to: page - 1)
        }
        pi.onSeqChanged = {
            Task {
                await store.refresh()
                if let fresh = store.docs.first(where: { $0.id == doc.id }), !fresh.seq.isEmpty {
                    seq = fresh.seq
                }
            }
        }
    }

    private var currentModel: PageModel? { models[seq[index].inkKey] }

    private var syncBadge: some View {
        Group {
            switch currentModel?.sync {
            case .dirty, .saving: Image(systemName: "arrow.triangle.2.circlepath").foregroundStyle(.orange)
            case .saved: Image(systemName: "checkmark.icloud").foregroundStyle(.green)
            case .error: Image(systemName: "exclamationmark.icloud").foregroundStyle(.red)
            default: EmptyView()
            }
        }
        .font(.footnote)
        .animation(.default, value: currentModel?.sync)
    }

    // papier's right-edge toolbar, reinterpreted as a floating rail.
    private var toolRail: some View {
        VStack(spacing: 12) {
            // Same static working dot as the tablet, occupying the rail's top.
            Circle()
                .fill(Color.primary.opacity(0.62))
                .frame(width: 8, height: 8)
                .scaleEffect(pi.busy ? 1 : 0.25)
                .opacity(pi.busy ? 1 : 0)
                .blur(radius: pi.busy ? 0 : 4)
                .animation(.easeOut(duration: 0.22), value: pi.busy)
                .accessibilityIdentifier("rail-busy-dot")
                .accessibilityLabel("Pi working")
                .accessibilityHidden(!pi.busy)

            railButton("pencil", active: tool == .pencil) { tool = .pencil }
                .accessibilityIdentifier("rail-pencil")
            // eraser: tap to select; tap again to cycle Object -> Pixel -> Region
            railButton(tool == .eraser ? eraserMode.symbol : "eraser", active: tool == .eraser) {
                if tool == .eraser { eraserMode = eraserMode.next } else { tool = .eraser }
            }
            .accessibilityIdentifier("rail-eraser")
            railButton("lasso", active: tool == .lasso) { tool = .lasso }
                .accessibilityIdentifier("rail-lasso")
            railButton("hand.draw", active: fingerDraws) { fingerDraws.toggle() }
                .accessibilityIdentifier("rail-finger")
            Divider().frame(width: 22)
            railButton("arrow.uturn.backward", active: false) {
                hub.activeCanvas?.undoManager?.undo()
            }
            railButton("arrow.uturn.forward", active: false) {
                hub.activeCanvas?.undoManager?.redo()
            }
            Divider().frame(width: 22)
            railButton("chevron.up", active: false) { changePage(-1) }
            railButton("number", active: false) { askGoTo = true }
                .accessibilityIdentifier("rail-goto")
            railButton("chevron.down", active: false) { changePage(1) }
            Divider().frame(width: 22)
            // Exact tablet glyphs: block-pixel Pi mode, then hand-squiggle Nudge.
            papierRailButton(.pi, active: pi.mode == "auto") { pi.toggleMode() }
                .accessibilityIdentifier("rail-pimode")
                .accessibilityLabel(pi.mode == "auto" ? "Pi automatic" : "Pi quiet")
            papierRailButton(.nudge, active: false) { pi.nudge(page: index + 1) }
                .accessibilityIdentifier("rail-nudge")
                .accessibilityLabel("Nudge Pi")
            Button { pi.cycleFont() } label: {
                Text(String(pi.font.prefix(2)).capitalized)
                    .font(.system(size: 13, weight: .semibold, design: .serif))
                    .frame(width: 30, height: 30)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("rail-pifont")
        }
        .padding(.vertical, 14)
        .padding(.horizontal, 8)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
        .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
    }

    private enum PapierRailIcon { case pi, nudge }

    private func papierRailButton(_ icon: PapierRailIcon, active: Bool,
                                  action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Group {
                switch icon {
                case .pi:
                    PapierPiGlyph().fill(Color.primary)
                        .frame(width: 20, height: 25)
                case .nudge:
                    VStack(spacing: -2) {
                        Image(systemName: "arrow.up")
                            .font(.system(size: 10, weight: .bold))
                        Text("NUDGE")
                            .font(.system(size: 7, weight: .bold, design: .rounded))
                            .tracking(-0.35)
                    }
                    .foregroundStyle(Color.primary)
                    .frame(width: 30, height: 26)
                }
            }
            .frame(width: 30, height: 30)
            .background(active ? Color.accentColor.opacity(0.18) : .clear,
                        in: RoundedRectangle(cornerRadius: 8))
            .frame(width: 40, height: 40)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func railButton(_ symbol: String, active: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 17, weight: .medium))
                .frame(width: 30, height: 30)
                .background(active ? Color.accentColor.opacity(0.18) : .clear,
                            in: RoundedRectangle(cornerRadius: 8))
                .frame(width: 40, height: 40)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func togglePencilEraser() {
        withAnimation(.easeOut(duration: 0.16)) {
            tool = tool == .pencil ? .eraser : .pencil
        }
    }

    private func handlePageDrag(_ phase: PageDragPhase) {
        guard !flipAnimating else { return }
        switch phase {
        case .changed(let translation):
            flipTarget = nil
            let beyondFirst = index == 0 && translation > 0
            let beyondLast = index == seq.count - 1 && translation < 0
            dragX = (beyondFirst || beyondLast) ? translation * 0.25 : translation
        case .ended(let translation, let velocity):
            let threshold = pageWidth * 0.18
            let next = dragX < 0 && (translation < -threshold || velocity < -600)
            let previous = dragX > 0 && (translation > threshold || velocity > 600)
            if next && index + 1 < seq.count {
                commitPageFlip(1, target: index + 1)
            } else if previous && index > 0 {
                commitPageFlip(-1, target: index - 1)
            } else {
                withAnimation(.spring(duration: 0.3)) { dragX = 0 }
            }
        case .cancelled:
            withAnimation(.spring(duration: 0.3)) { dragX = 0 }
        }
    }

    private func changePage(_ delta: Int) {
        commitPageFlip(delta, target: index + delta)
    }

    private func animatePage(to target: Int) {
        guard target != index, seq.indices.contains(target) else { return }
        commitPageFlip(target > index ? 1 : -1, target: target)
    }

    private func commitPageFlip(_ direction: Int, target: Int) {
        guard !flipAnimating, seq.indices.contains(target), target != index else {
            withAnimation(.spring(duration: 0.3)) { dragX = 0 }
            return
        }
        flipAnimating = true
        flipTarget = target
        currentModel?.flushNow()
        withAnimation(.easeOut(duration: 0.24)) {
            dragX = CGFloat(-direction) * (pageWidth + pageGap)
        } completion: {
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) {
                index = target
                dragX = 0
                flipTarget = nil
            }
            flipAnimating = false
        }
    }

    private func flushAll() {
        for m in models.values { m.flushNow() }
    }

    /// Append a fresh note page after the current one and tell the cloud.
    private func addPage() {
        let nextNote = (seq.compactMap { if case .note(let n) = $0 { n } else { nil } }.max() ?? 0) + 1
        seq.insert(.note(nextNote), at: index + 1)
        let state = DocState(nextNote: nextNote + 1, pos: index + 1, seq: seq)
        commitPageFlip(1, target: index + 1)
        Task { try? await store.client.postState(docId: doc.id, state: state) }
    }
}

// MARK: - one page

@MainActor
private final class PageModelCache: ObservableObject {
    private var storage: [String: PageModel] = [:]

    func model(doc: PapierDoc, entry: SeqEntry, store: LibraryStore) -> PageModel {
        if let existing = storage[entry.inkKey] { return existing }
        let created = PageModel(doc: doc, entry: entry, store: store)
        storage[entry.inkKey] = created
        return created
    }

    subscript(key: String) -> PageModel? { storage[key] }
    var values: Dictionary<String, PageModel>.Values { storage.values }
}

private struct PageScreen: View {
    let doc: PapierDoc
    let entry: SeqEntry
    @ObservedObject var model: PageModel
    let near: Bool
    let active: Bool
    let tool: CanvasTool
    let eraserMode: EraserMode
    let fingerDraws: Bool
    let hub: CanvasHub
    let onPencilDoubleTap: () -> Void
    let onPageDrag: (PageDragPhase) -> Void

    @EnvironmentObject private var store: LibraryStore
    @Environment(\.colorScheme) private var colorScheme
    @AppStorage("paperTone") private var paperToneRaw = PaperTone.paper.rawValue
    @State private var selection: InkSelection?

    private var paper: Color {
        (PaperTone(rawValue: paperToneRaw) ?? .paper).color(dark: colorScheme == .dark)
    }

    /// A capture overlay owns the touches while lassoing or region-erasing.
    private var capturing: Bool {
        active && ((tool == .lasso && selection == nil) || (tool == .eraser && eraserMode == .region))
    }

    var body: some View {
        GeometryReader { geo in
            let fit = fittedSize(in: geo.size)
            ZStack {
                if near {
                    page(fit: fit)
                        // multiply tints the page: raster/patch whites become
                        // the paper tone, ink stays dark
                        .colorMultiply(paper)
                        .frame(width: fit.width, height: fit.height)
                        .background(paper)
                        // Hard page boundary: vector text/ink can never paint
                        // into the desk or underneath the tool rail.
                        .clipped()
                        .shadow(color: .black.opacity(0.14), radius: 8, y: 2)
                        .task(id: fit.width) {
                            await model.load(displayWidth: fit.width)
                            model.rescale(displayWidth: fit.width)
                        }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // .contain keeps children (pi-patch-layer) visible to XCUI.
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier(active ? "page-surface" : "neighbor-page")
            .onChange(of: tool) { _, _ in selection = nil }
        }
    }

    @ViewBuilder
    private func page(fit: CGSize) -> some View {
        ZStack {
            Color.white
            if case .pdf(let p) = entry {
                AsyncImage(url: store.client.pageURL(doc, pdfPage: p)) { phase in
                    switch phase {
                    case .success(let img): img.resizable().scaledToFit()
                    case .empty: ProgressView()
                    case .failure: Image(systemName: "photo").foregroundStyle(.tertiary)
                    @unknown default: EmptyView()
                    }
                }
            }
            PatchLayer(patches: model.patches,
                       scale: model.scale,
                       animateIds: model.animateIds,
                       animateStart: model.animateStart)
            if let drawing = model.initialDrawing {
                CanvasView(initialDrawing: drawing,
                           epoch: model.drawingEpoch,
                           tool: tool,
                           eraserMode: eraserMode,
                           fingerDraws: fingerDraws,
                           interactionEnabled: active && !capturing && selection == nil,
                           isActive: active,
                           hub: hub,
                           onChanged: { model.drawingChanged($0) },
                           onPencilDoubleTap: onPencilDoubleTap,
                           onPageDrag: onPageDrag,
                           onErase: { point in
                               model.erasePiInk(atDisplayPoint: point, mode: eraserMode)
                           })
            } else {
                ProgressView()
            }
            if capturing {
                CaptureView(dashed: true) { poly in
                    if tool == .lasso { lassoCompleted(poly) } else { regionErase(poly) }
                }
            }
            if let sel = selection {
                selectionOverlay(sel)
            }
        }
    }

    // MARK: - lasso

    private func lassoCompleted(_ poly: [CGPoint]) {
        let drawing = model.currentDrawing
        let strokes = InkGeometry.strokesInside(drawing, poly: poly)
        let patches = InkGeometry.patchesInside(model.patches, poly: poly, scale: model.scale)
        guard !strokes.isEmpty || !patches.isEmpty else { return }
        selection = InkSelection(strokeIndices: strokes, patchIds: patches,
                                 bbox: InkGeometry.bounds(of: poly))
    }

    private func regionErase(_ poly: [CGPoint]) {
        let drawing = model.currentDrawing
        let doomed = Set(InkGeometry.strokesInside(drawing, poly: poly))
        if !doomed.isEmpty {
            let kept = drawing.strokes.enumerated().filter { !doomed.contains($0.offset) }.map(\.element)
            model.setDrawing(PKDrawing(strokes: kept))
        }
        model.erasePatches(ids: InkGeometry.patchesInside(model.patches, poly: poly, scale: model.scale))
    }

    private func selectionOverlay(_ sel: InkSelection) -> some View {
        let rect = sel.bbox.offsetBy(dx: sel.offset.width, dy: sel.offset.height)
        return ZStack(alignment: .topTrailing) {
            RoundedRectangle(cornerRadius: 6)
                .stroke(Color.accentColor, style: StrokeStyle(lineWidth: 1.5, dash: [6, 4]))
                .background(Color.accentColor.opacity(0.06), in: RoundedRectangle(cornerRadius: 6))
                .frame(width: rect.width, height: rect.height)
                .position(x: rect.midX, y: rect.midY)
                .gesture(
                    DragGesture()
                        .onChanged { v in selection?.offset = v.translation }
                        .onEnded { v in applyMove(v.translation) }
                )
            HStack(spacing: 10) {
                Button { deleteSelection() } label: {
                    Image(systemName: "trash").font(.system(size: 15, weight: .medium))
                }
                Button { selection = nil } label: {
                    Image(systemName: "xmark").font(.system(size: 15, weight: .medium))
                }
            }
            .padding(8)
            .background(.regularMaterial, in: Capsule())
            .position(x: rect.midX, y: max(22, rect.minY - 26))
        }
    }

    private func applyMove(_ delta: CGSize) {
        guard let sel = selection, delta != .zero else { selection?.offset = .zero; return }
        if !sel.strokeIndices.isEmpty {
            var strokes = model.currentDrawing.strokes
            let t = CGAffineTransform(translationX: delta.width, y: delta.height)
            for i in sel.strokeIndices where i < strokes.count {
                strokes[i].transform = strokes[i].transform.concatenating(t)
            }
            model.setDrawing(PKDrawing(strokes: strokes))
        }
        model.movePatches(ids: sel.patchIds, by: delta)
        selection = nil
    }

    private func deleteSelection() {
        guard let sel = selection else { return }
        if !sel.strokeIndices.isEmpty {
            let doomed = Set(sel.strokeIndices)
            let kept = model.currentDrawing.strokes.enumerated()
                .filter { !doomed.contains($0.offset) }.map(\.element)
            model.setDrawing(PKDrawing(strokes: kept))
        }
        model.erasePatches(ids: sel.patchIds)
        selection = nil
    }

    private func fittedSize(in container: CGSize) -> CGSize {
        let aspect = doc.pageW / doc.pageH
        let margin: CGFloat = 12
        let w = min(container.width - margin * 2, (container.height - margin * 2) * aspect)
        return CGSize(width: max(w, 1), height: max(w / aspect, 1))
    }
}

