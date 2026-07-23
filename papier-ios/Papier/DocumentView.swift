// DocumentView.swift — a document open full-screen: page-curl (or arrow) page
// navigation over the doc's seq, a papier-style floating right-edge tool
// rail (pencil / eraser / finger / undo / redo), page counter, add-page
// for notebooks. Books show the pre-rendered raster under the ink.
//
// Page turning is CurlPager — the UIKit .pageCurl deck. The paper bends
// under the finger like Apple Books; commit/cancel physics, backside
// render and fold shadows are all system-drawn.

import PencilKit
import SwiftUI

struct DocumentView: View {
    let doc: PapierDoc

    @EnvironmentObject private var store: LibraryStore
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var colorScheme

    @State private var seq: [SeqEntry]
    @State private var index: Int
    @State private var tool: CanvasTool = .pencil
    @State private var eraserMode: EraserMode = .object
    @State private var fingerDraws = false
    @State private var askGoTo = false
    @State private var goToText = ""
    @AppStorage("pageTurnStyle") private var pageTurnStyleRaw = PageTurnStyle.curl.rawValue
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

    /// Where the user parked the toolbar. Papier-tablet behavior: grab it,
    /// dock it on any edge, or collapse it to a chip. Remembered forever.
    private enum ToolbarEdge: String {
        case top, bottom, leading, trailing

        var alignment: Alignment {
            switch self {
            case .top: return .top
            case .bottom: return .bottom
            case .leading: return .leading
            case .trailing: return .trailing
            }
        }

        var isVertical: Bool { self == .leading || self == .trailing }
    }

    @AppStorage("toolbarEdge") private var toolbarEdgeRaw = ToolbarEdge.trailing.rawValue
    @AppStorage("toolbarMinimized") private var toolbarMinimized = false
    /// How far along the edge (from the bar's center) the minimize control
    /// sat when the bar collapsed — the chip parks exactly there, so
    /// minimize and maximize live under the same finger.
    @AppStorage("toolbarAlong") private var toolbarAlong: Double = 0
    @State private var toolbarDrag: CGSize = .zero
    @State private var expandedBarSize: CGSize = .zero

    private var toolbarEdge: ToolbarEdge { ToolbarEdge(rawValue: toolbarEdgeRaw) ?? .trailing }

    /// The doc has one aspect for every page; the curl deck is framed to
    /// exactly the fitted paper so the bend happens on the page itself,
    /// never on the surrounding desk. Margins are shaved to 4pt, the nav
    /// bar is gone, and the toolbar floats wherever the user parked it —
    /// the paper always gets the maximum fit.
    private func fittedPageSize(in container: CGSize) -> CGSize {
        let aspect = doc.pageW / doc.pageH
        let margin: CGFloat = 4
        let w = min(container.width - margin * 2, (container.height - margin * 2) * aspect)
        return CGSize(width: max(w, 1), height: max(w / aspect, 1))
    }

    /// Drag the grip (or the chip): follow the finger, then snap to the
    /// nearest screen edge on release and remember it. Tool taps never
    /// touch this gesture — it lives ONLY on the grip and the chip.
    private func dockGesture(in size: CGSize, minimum: CGFloat) -> some Gesture {
        DragGesture(minimumDistance: minimum, coordinateSpace: .named("desk"))
            .onChanged { toolbarDrag = $0.translation }
            .onEnded { value in
                // A dock change takes a REAL, deliberate drag — jitter and
                // brushes must never teleport the toolbar.
                guard hypot(value.translation.width, value.translation.height) > 40 else {
                    withAnimation(.spring(duration: 0.3)) { toolbarDrag = .zero }
                    return
                }
                let p = value.location
                let candidates: [(ToolbarEdge, CGFloat)] = [
                    (.leading, p.x),
                    (.trailing, size.width - p.x),
                    (.top, p.y),
                    (.bottom, size.height - p.y),
                ]
                let nearest = candidates.min { $0.1 < $1.1 }?.0 ?? .trailing
                withAnimation(.spring(duration: 0.35)) {
                    toolbarEdgeRaw = nearest.rawValue
                    toolbarDrag = .zero
                }
            }
    }

    private var turnStyle: PageTurnStyle {
        PageTurnStyle(rawValue: pageTurnStyleRaw) ?? .curl
    }

    @ViewBuilder
    private func curlDeck(fit: CGSize) -> some View {
        CurlPager(style: turnStyle,
                  count: seq.count,
                  index: $index,
                  pageKey: { seq[$0].inkKey },
                  makePage: { pageScreen(at: $0) },
                  makeBack: { _ in PageBackView() },
                  edgeOnlyCurl: fingerDraws)
            .frame(width: fit.width, height: fit.height)
            .shadow(color: .black.opacity(0.14), radius: 8, y: 2)
            // transitionStyle is fixed at creation — rebuild the deck when
            // the Settings choice changes.
            .id(turnStyle)
    }

    // MARK: - dockable toolbar pieces

    private var backChip: some View {
        Button { dismiss() } label: {
            Image(systemName: "chevron.left")
                .font(.system(size: 16, weight: .semibold))
                .frame(width: 34, height: 34)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Back to library")
    }

    private var counterButton: some View {
        Button { askGoTo = true } label: {
            Text("\(index + 1) / \(seq.count)")
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .minimumScaleFactor(0.6)
        }
        .buttonStyle(.plain)
        // XCUI (PagerFlowTests) matches the counter as a staticText;
        // .contain keeps the child text visible through the button.
        .accessibilityElement(children: .contain)
        .help("Go to page")
    }

    private var addButton: some View {
        Button { addPage() } label: {
            Image(systemName: "plus.square.on.square")
                .font(.system(size: 15, weight: .medium))
                .frame(width: 34, height: 34)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Insert a note page after this one")
    }

    private var minimizeButton: some View {
        Button {
            // The chip takes the minimize button's own spot: half the bar
            // length minus the button's inset from the bar's TOP/LEADING
            // end (negative = toward the start of the edge).
            let length = toolbarEdge.isVertical ? expandedBarSize.height : expandedBarSize.width
            toolbarAlong = -Double(max(0, length / 2 - 26))
            withAnimation(.spring(duration: 0.3)) { toolbarMinimized = true }
        } label: {
            Image(systemName: "rectangle.compress.vertical")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(.secondary)
                .frame(width: 30, height: 30)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Minimize tools")
        .accessibilityIdentifier("rail-minimize")
    }

    /// The grip — the ONLY place the expanded bar can be grabbed and
    /// dragged to another edge. Tapping tools can never move the bar.
    private func gripHandle(in size: CGSize) -> some View {
        Image(systemName: "line.3.horizontal")
            .font(.system(size: 12, weight: .semibold))
            .foregroundStyle(.tertiary)
            .frame(width: 34, height: 26)
            .contentShape(Rectangle())
            .gesture(dockGesture(in: size, minimum: 8))
            .accessibilityLabel("Move toolbar")
            .accessibilityIdentifier("rail-grip")
    }

    @ViewBuilder
    private func dockedToolbar(in size: CGSize) -> some View {
        Group {
            if toolbarMinimized {
                minimizedChip(in: size)
            } else {
                expandedToolbar(in: size)
            }
        }
        .offset(toolbarDrag)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: toolbarEdge.alignment)
        // Inset past the desk gap so bar and chip sit FULLY on the paper,
        // never straddling the page's edge line.
        .padding(12)
    }

    /// Collapsed: the tablet's single dot. The dot IS the pi light —
    /// quiet gray normally, the rail blue while pi is working. Tap to
    /// expand, drag to re-dock — exactly like the tablet.
    private func minimizedChip(in size: CGSize) -> some View {
        Circle()
            .fill(pi.busy ? railBlue : Color.primary.opacity(0.5))
            .frame(width: 7, height: 7)
            .animation(.easeOut(duration: 0.25), value: pi.busy)
            .frame(width: 40, height: 40)
            .background(.regularMaterial, in: Circle())
            .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
            .contentShape(Circle())
            .onTapGesture {
                withAnimation(.spring(duration: 0.3)) { toolbarMinimized = false }
            }
            .gesture(dockGesture(in: size, minimum: 12))
            // Park at the minimize button's former position along the edge —
            // NOT the edge's center — so collapse/expand toggle in one place.
            .offset(x: toolbarEdge.isVertical ? 0 : toolbarAlong,
                    y: toolbarEdge.isVertical ? toolbarAlong : 0)
            .accessibilityLabel("Show tools")
            .accessibilityIdentifier("rail-expand")
    }

    @ViewBuilder
    private func expandedToolbar(in size: CGSize) -> some View {
        if toolbarEdge.isVertical {
            VStack(spacing: 8) {
                minimizeButton
                gripHandle(in: size)
                backChip
                busyDot
                railDivider(vertical: true)
                railControls(vertical: true)
                railDivider(vertical: true)
                syncBadge
                counterButton
                addButton
            }
            .padding(.vertical, 8)
            .padding(.horizontal, 5)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
            .background(GeometryReader { g in
                Color.clear
                    .onAppear { expandedBarSize = g.size }
                    .onChange(of: g.size) { _, s in expandedBarSize = s }
            })
            .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
        } else {
            let bar = HStack(spacing: 6) {
                minimizeButton
                gripHandle(in: size)
                backChip
                busyDot
                railDivider(vertical: false)
                railControls(vertical: false)
                railDivider(vertical: false)
                syncBadge
                counterButton
                addButton
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)

            ViewThatFits(in: .horizontal) {
                bar
                ScrollView(.horizontal, showsIndicators: false) { bar }
            }
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
            .background(GeometryReader { g in
                Color.clear
                    .onAppear { expandedBarSize = g.size }
                    .onChange(of: g.size) { _, s in expandedBarSize = s }
            })
            .shadow(color: .black.opacity(0.12), radius: 6, y: 2)
        }
    }

    private func pageScreen(at pageIndex: Int) -> some View {
        let entry = seq[pageIndex]
        return PageScreen(doc: doc,
                          entry: entry,
                          model: model(for: entry),
                          active: pageIndex == index,
                          tool: tool,
                          eraserMode: eraserMode,
                          fingerDraws: fingerDraws,
                          hub: hub,
                          onPencilDoubleTap: togglePencilEraser)
            // ONE id per page always: at curl commit the neighbor sheet
            // BECOMES the active sheet in place. Distinct neighbor/active
            // ids rebuilt the page after landing — raster refetch + canvas
            // recreation = the visible flash.
            .id(entry.inkKey)
            // Hosting controllers live outside the SwiftUI tree; the
            // environment must ride along explicitly.
            .environmentObject(store)
    }

    var body: some View {
        GeometryReader { geo in
            let fit = fittedPageSize(in: geo.size)
            ZStack {
                Color(uiColor: .systemGray6).ignoresSafeArea()

                // The Books deck: finger drags peel the paper, pencil never
                // turns a page. In finger-draw mode only the page margins
                // start a curl.
                curlDeck(fit: fit)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)

                // The papier toolbar: floats over the desk (and the page —
                // by choice), parks on any edge, collapses to a chip.
                dockedToolbar(in: geo.size)

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
            .coordinateSpace(name: "desk")
        }
        // The nav bar is gone — the paper owns the screen; chrome floats.
        .toolbar(.hidden, for: .navigationBar)
        .navigationBarBackButtonHidden(true)
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

    /// ONE blue for everything "on" about the rail — the active-tool
    /// highlight and the pi activity dot share it. Dark mode runs slightly
    /// lighter so it reads on the near-black rail material.
    private var railBlue: Color {
        colorScheme == .dark
            ? Color(red: 0.45, green: 0.69, blue: 1.00)
            : Color.accentColor
    }

    /// The rail's "this is on" tint. Plain accent blue at 18% disappears
    /// into the dark rail material; dark mode gets more presence so the
    /// active tool reads at a glance.
    private var activeFill: Color {
        railBlue.opacity(colorScheme == .dark ? 0.34 : 0.18)
    }

    /// Same static working dot as the tablet — the selection-highlight
    /// blue, so "pi is thinking" and "this is active" share one color.
    /// Visible ONLY while pi is actually working (busy), never parked.
    private var busyDot: some View {
        Circle()
            .fill(railBlue)
            .frame(width: 8, height: 8)
            .scaleEffect(pi.busy ? 1 : 0.25)
            .opacity(pi.busy ? 1 : 0)
            .blur(radius: pi.busy ? 0 : 4)
            .animation(.easeOut(duration: 0.22), value: pi.busy)
            .accessibilityIdentifier("rail-busy-dot")
            .accessibilityLabel("Pi working")
            .accessibilityHidden(!pi.busy)
    }

    @ViewBuilder
    private func railDivider(vertical: Bool) -> some View {
        if vertical {
            Divider().frame(width: 20)
        } else {
            Divider().frame(height: 20)
        }
    }

    /// The tool set, axis-agnostic — the landscape rail and the portrait
    /// bottom bar share every control and accessibility identifier.
    @ViewBuilder
    private func railControls(vertical: Bool) -> some View {
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
        railDivider(vertical: vertical)
        railButton("arrow.uturn.backward", active: false) {
            hub.activeCanvas?.undoManager?.undo()
        }
        railButton("arrow.uturn.forward", active: false) {
            hub.activeCanvas?.undoManager?.redo()
        }
        railDivider(vertical: vertical)
        railButton(vertical ? "chevron.up" : "chevron.left", active: false) { changePage(-1) }
        railButton("number", active: false) { askGoTo = true }
            .accessibilityIdentifier("rail-goto")
        railButton(vertical ? "chevron.down" : "chevron.right", active: false) { changePage(1) }
        railDivider(vertical: vertical)
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
            .background(active ? activeFill : .clear,
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
                .background(active ? activeFill : .clear,
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

    private func changePage(_ delta: Int) {
        animatePage(to: index + delta)
    }

    /// Setting `index` IS the navigation: CurlPager sees the binding move
    /// and plays the same curl a finger drag would.
    private func animatePage(to target: Int) {
        guard target != index, seq.indices.contains(target) else { return }
        currentModel?.flushNow()
        index = target
    }

    private func flushAll() {
        for m in models.values { m.flushNow() }
    }

    /// Append a fresh note page after the current one and tell the cloud.
    private func addPage() {
        let nextNote = (seq.compactMap { if case .note(let n) = $0 { n } else { nil } }.max() ?? 0) + 1
        seq.insert(.note(nextNote), at: index + 1)
        let state = DocState(nextNote: nextNote + 1, pos: index + 1, seq: seq)
        currentModel?.flushNow()
        index += 1
        Task { try? await store.client.postState(docId: doc.id, state: state) }
    }
}

/// The reverse side of a turning leaf. Real paper's back is the same
/// stock as its face: the current tone (dimmed further in dark mode),
/// with a soft cross-sheet shading so the curl reads as curvature —
/// never UIKit's default white-washed mirror.
private struct PageBackView: View {
    @Environment(\.colorScheme) private var colorScheme
    @AppStorage("paperTone") private var paperToneRaw = PaperTone.paper.rawValue

    private var paper: Color {
        (PaperTone(rawValue: paperToneRaw) ?? .paper).color(dark: colorScheme == .dark)
    }

    var body: some View {
        paper
            .overlay {
                LinearGradient(
                    stops: [
                        .init(color: .black.opacity(0.10), location: 0.00),
                        .init(color: .clear, location: 0.30),
                        .init(color: .clear, location: 0.72),
                        .init(color: .black.opacity(0.06), location: 1.00),
                    ],
                    startPoint: .leading,
                    endPoint: .trailing
                )
            }
            .ignoresSafeArea()
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
    let active: Bool
    let tool: CanvasTool
    let eraserMode: EraserMode
    let fingerDraws: Bool
    let hub: CanvasHub
    let onPencilDoubleTap: () -> Void

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
        // The curl deck frames each page controller to the fitted paper size,
        // so the page IS the container. The bend always happens on paper.
        GeometryReader { geo in
            let fit = CGSize(width: max(geo.size.width, 1), height: max(geo.size.height, 1))
            ZStack {
                page(fit: fit)
                    // multiply tints the page: raster/patch whites become
                    // the paper tone, ink stays dark
                    .colorMultiply(paper)
                    .frame(width: fit.width, height: fit.height)
                    .background(paper)
                    // Hard page boundary: vector text/ink can never paint
                    // into the desk or underneath the tool rail. It also
                    // keeps the curled leaf's raster edge-clean.
                    .clipped()
                    .task(id: fit.width) {
                        guard fit.width > 1 else { return }
                        await model.load(displayWidth: fit.width)
                        model.rescale(displayWidth: fit.width)
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
}
