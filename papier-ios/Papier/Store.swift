// Store.swift — library state + the local pending-ink cache.
//
// Pending ink: when the iPad writes a page, the write goes to the VM's
// inbound tree, but the mirror copy (what reads serve) stays stale until
// the tablet wakes, pulls, and pushes back. Until the doc's mirror version
// moves past our write, the local copy is the freshest truth and wins.

import Foundation
import SwiftUI

@MainActor
final class LibraryStore: ObservableObject {
    @AppStorage("serverRoot") var serverRoot: String = "" {
        didSet { etag = nil }
    }

    @Published var docs: [PapierDoc] = []
    @Published var generation: String = ""
    @Published var lastError: String?
    @Published var loading = false

    private var etag: String?
    private var pollTask: Task<Void, Never>?

    var client: PapierClient { PapierClient(serverRoot: serverRoot.trimmingCharacters(in: .whitespaces)) }
    var configured: Bool { serverRoot.contains("://") }

    func refresh() async {
        guard configured else { return }
        loading = docs.isEmpty
        defer { loading = false }
        do {
            let (lib, newTag) = try await client.library(etag: etag)
            etag = newTag
            if let lib {
                docs = lib.docs.sorted { $0.meta.title.localizedCaseInsensitiveCompare($1.meta.title) == .orderedAscending }
                generation = lib.generation
                prunePending(docs: lib.docs)
            }
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
    }

    /// Matches the web viewer: poll the ETagged manifest every 60s while
    /// the home view is visible; callers cancel when a document is open.
    func startPolling() {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(for: .seconds(60))
            }
        }
    }

    func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    // MARK: - pending ink cache (Documents/pending/<docid>/<key>.json)

    private static var pendingDir: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("pending", isDirectory: true)
    }

    private func pendingURL(_ docId: String, _ key: String) -> URL {
        Self.pendingDir.appendingPathComponent(docId, isDirectory: true)
            .appendingPathComponent(key + ".json")
    }

    /// Record a successful upload: keep the page locally, stamped with the
    /// doc version it was based on, until the mirror version moves.
    func rememberPending(docId: String, key: String, page: InkPage, baseVersion: String) {
        let url = pendingURL(docId, key)
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(),
                                                 withIntermediateDirectories: true)
        var obj = (try? JSONSerialization.jsonObject(with: page.serialized())) as? [String: Any] ?? [:]
        obj["_baseVersion"] = baseVersion
        if let data = try? JSONSerialization.data(withJSONObject: obj) {
            try? data.write(to: url, options: .atomic)
        }
    }

    /// The freshest known ink for a page: the local pending copy while the
    /// mirror still shows the version we based that copy on.
    func pendingInk(docId: String, key: String, currentVersion: String) -> InkPage? {
        let url = pendingURL(docId, key)
        guard let data = try? Data(contentsOf: url),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let base = obj["_baseVersion"] as? String
        if base != nil && base != currentVersion {
            // The doc changed server-side since our write (tablet round-trip
            // or another writer): the server is truth again.
            try? FileManager.default.removeItem(at: url)
            return nil
        }
        return InkPage.parse(data)
    }

    private func prunePending(docs: [PapierDoc]) {
        let fm = FileManager.default
        guard let dirs = try? fm.contentsOfDirectory(at: Self.pendingDir,
                                                     includingPropertiesForKeys: nil) else { return }
        let byId = Dictionary(uniqueKeysWithValues: docs.map { ($0.id, $0) })
        for dir in dirs where byId[dir.lastPathComponent] == nil {
            try? fm.removeItem(at: dir)   // doc deleted server-side
        }
    }
}
