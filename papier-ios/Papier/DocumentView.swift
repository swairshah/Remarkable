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

    @State private var seq: [SeqEntry]
    @State private var index: Int
    @State private var tool: CanvasTool = .pencil
    @State private var fingerDraws = false
    @State private var models: [String: PageModel] = [:]
    @StateObject private var hub = CanvasHub()
    @StateObject private var pi: PiSession

    init(doc: PapierDoc) {
        self.doc = doc
        _seq = State(initialValue: doc.seq.isEmpty ? [.note(1)] : doc.seq)
        let saved = UserDefaults.standard.integer(forKey: "pos-\(doc.id)")
        _index = State(initialValue: min(max(saved, 0), max((doc.seq.count) - 1, 0)))
        _pi = StateObject(wrappedValue: PiSession(
            docId: doc.id,
            serverRoot: UserDefaults.standard.string(forKey: "serverRoot") ?? ""))
    }

    private func model(for entry: SeqEntry) -> PageModel {
        if let m = models[entry.inkKey] { return m }
        let m = PageModel(doc: doc, entry: entry, store: store)
        m.onSaved = { [weak pi] in
            // the debounced save just landed — that IS the writing pause
            if let i = seq.firstIndex(of: entry) { pi?.pause(page: i + 1) }
        }
        DispatchQueue.main.async { models[entry.inkKey] = m }
        return m
    }

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color(uiColor: .systemGray6).ignoresSafeArea()

                TabView(selection: $index) {
                    ForEach(Array(seq.enumerated()), id: \.offset) { i, entry in
                        PageScreen(doc: doc,
                                   entry: entry,
                                   model: model(for: entry),
                                   near: abs(i - index) <= 1,
                                   active: i == index,
                                   tool: tool,
                                   fingerDraws: fingerDraws,
                                   hub: hub)
                            .tag(i)
                    }
                }
                .tabViewStyle(.page(indexDisplayMode: .never))
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
            ToolbarItemGroup(placement: .topBarTrailing) {
                syncBadge
                Text("\(index + 1) / \(seq.count)")
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(.secondary)
                if doc.isNotebook {
                    Button { addPage() } label: { Image(systemName: "plus.square.on.square") }
                        .help("Add a page after this one")
                }
            }
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
        pi.onPatch = { page in
            guard page >= 1, page <= seq.count else { return }
            let key = seq[page - 1].inkKey
            Task { await models[key]?.refreshPatches() }
        }
        pi.onGoto = { page in
            guard page >= 1, page <= seq.count else { return }
            withAnimation { index = page - 1 }
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
        VStack(spacing: 14) {
            railButton("pencil", active: tool == .pencil) { tool = .pencil }
                .accessibilityIdentifier("rail-pencil")
            railButton("eraser", active: tool == .eraser) { tool = .eraser }
                .accessibilityIdentifier("rail-eraser")
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
            // pi: busy dot / nudge / quiet toggle / pi's writing face
            ZStack {
                railButton("sparkles", active: false) { pi.nudge(page: index + 1) }
                    .accessibilityIdentifier("rail-nudge")
                    .opacity(pi.busy ? 0.25 : 1)
                if pi.busy { ProgressView().controlSize(.small) }
            }
            railButton(pi.mode == "quiet" ? "moon.zzz.fill" : "moon.zzz", active: pi.mode == "quiet") {
                pi.toggleMode()
            }
            .accessibilityIdentifier("rail-pimode")
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

    private func railButton(_ symbol: String, active: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.system(size: 17, weight: .medium))
                .frame(width: 30, height: 30)
                .background(active ? Color.accentColor.opacity(0.18) : .clear,
                            in: RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
    }

    private func flushAll() {
        for m in models.values { m.flushNow() }
    }

    /// Append a fresh note page after the current one and tell the cloud.
    private func addPage() {
        let nextNote = (seq.compactMap { if case .note(let n) = $0 { n } else { nil } }.max() ?? 0) + 1
        seq.insert(.note(nextNote), at: index + 1)
        let state = DocState(nextNote: nextNote + 1, pos: index + 1, seq: seq)
        Task {
            try? await store.client.postState(docId: doc.id, state: state)
            withAnimation { index += 1 }
        }
    }
}

// MARK: - one page

private struct PageScreen: View {
    let doc: PapierDoc
    let entry: SeqEntry
    @ObservedObject var model: PageModel
    let near: Bool
    let active: Bool
    let tool: CanvasTool
    let fingerDraws: Bool
    let hub: CanvasHub

    @EnvironmentObject private var store: LibraryStore
    @Environment(\.colorScheme) private var colorScheme
    @AppStorage("paperTone") private var paperToneRaw = PaperTone.paper.rawValue

    private var paper: Color {
        (PaperTone(rawValue: paperToneRaw) ?? .paper).color(dark: colorScheme == .dark)
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
                        .shadow(color: .black.opacity(0.14), radius: 8, y: 2)
                        .task(id: fit.width) {
                            await model.load(displayWidth: fit.width)
                            model.rescale(displayWidth: fit.width)
                        }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
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
                           fingerDraws: fingerDraws,
                           isActive: active,
                           hub: hub,
                           onChanged: { model.drawingChanged($0) })
            } else {
                ProgressView()
            }
        }
    }

    private func fittedSize(in container: CGSize) -> CGSize {
        let aspect = doc.pageW / doc.pageH
        let margin: CGFloat = 12
        let w = min(container.width - margin * 2, (container.height - margin * 2) * aspect)
        return CGSize(width: max(w, 1), height: max(w / aspect, 1))
    }
}
