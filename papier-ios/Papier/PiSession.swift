// PiSession.swift — the iPad's handle on remote pi (the session service on
// the papier VM). Reports pauses/nudges, polls events, and surfaces pi's
// activity: busy dot, arriving patches, page turns, and notice toasts.

import Foundation
import SwiftUI

struct PiEvent: Decodable {
    let id: Int
    let type: String     // turn | patch | seq | goto | notice
    let state: String?   // for turn: start | end
    let page: Int?
    let text: String?
}

private struct PiStateResponse: Decodable {
    let ok: Bool
    let busy: Bool
    let mode: String
    let font: String
    let epoch: Double?
    let events: [PiEvent]?
}

@MainActor
final class PiSession: ObservableObject {
    let docId: String
    /// Set from LibraryStore before open() — the SAME config the library
    /// uses (never read UserDefaults directly: @AppStorage defaults are
    /// not persisted, so a raw read returns nil on fresh installs).
    var serverRoot: String

    @Published var busy = false
    @Published var mode: String = "auto"     // auto | quiet
    @Published var font: String = "serif"
    @Published var toast: String?

    /// Wired by DocumentView.
    var onPatch: ((Int) -> Void)?    // page (1-based) got new pi ink
    var onGoto: ((Int) -> Void)?     // pi turned to page (1-based)
    var onSeqChanged: (() -> Void)?  // pi inserted a note page

    private var since = 0
    private var epoch: Double = 0
    private var pollTask: Task<Void, Never>?
    private var toastTask: Task<Void, Never>?

    init(docId: String, serverRoot: String) {
        self.docId = docId
        self.serverRoot = serverRoot
    }

    private func call(_ pathAndQuery: String, method: String = "POST") async -> PiStateResponse? {
        guard let url = URL(string: "\(serverRoot)/papier/api/pi/\(pathAndQuery)") else { return nil }
        var req = URLRequest(url: url)
        req.httpMethod = method
        req.timeoutInterval = 15
        guard let (data, resp) = try? await URLSession.shared.data(for: req),
              (resp as? HTTPURLResponse)?.statusCode == 200 else { return nil }
        return try? JSONDecoder().decode(PiStateResponse.self, from: data)
    }

    private func apply(_ st: PiStateResponse?) {
        guard let st else { return }
        // The service restarted: its event ids started over — reset the
        // cursor or we would silently filter every new event out.
        if let e = st.epoch, e != epoch {
            epoch = e
            since = 0
        }
        busy = st.busy
        mode = st.mode
        font = st.font
        for ev in st.events ?? [] {
            since = max(since, ev.id)
            switch ev.type {
            case "patch": if let p = ev.page { onPatch?(p) }
            case "goto": if let p = ev.page { onGoto?(p) }
            case "seq": onSeqChanged?()
            case "notice": if let t = ev.text, !t.isEmpty { show(toast: t) }
            case "turn": busy = (ev.state == "start")
            default: break
            }
        }
    }

    private func show(toast text: String) {
        toast = text
        toastTask?.cancel()
        toastTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(5))
            if !Task.isCancelled { self?.toast = nil }
        }
    }

    // MARK: - lifecycle

    func open() {
        Task { apply(await call("open?id=\(docId)")) }
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                let interval: Double = self.busy ? 2.0 : 5.0
                self.apply(await self.call("events?id=\(self.docId)&since=\(self.since)", method: "GET"))
                try? await Task.sleep(for: .seconds(interval))
            }
        }
    }

    func close() {
        pollTask?.cancel()
        pollTask = nil
    }

    // MARK: - controls

    func reportPage(_ page: Int) {
        Task { apply(await call("page?id=\(docId)&page=\(page)")) }
    }

    /// The user paused writing (called after a successful ink sync).
    func pause(page: Int) {
        guard mode == "auto" else { return }
        Task { apply(await call("pause?id=\(docId)&page=\(page)")) }
    }

    func nudge(page: Int) {
        busy = true
        Task { apply(await call("nudge?id=\(docId)&page=\(page)")) }
    }

    func toggleMode() {
        let next = mode == "auto" ? "quiet" : "auto"
        mode = next
        Task { apply(await call("mode?id=\(docId)&mode=\(next)")) }
    }

    func cycleFont() {
        let order = ["serif", "script", "sans", "garamond"]
        let next = order[((order.firstIndex(of: font) ?? 0) + 1) % order.count]
        font = next
        Task { apply(await call("font?id=\(docId)&font=\(next)")) }
    }
}
