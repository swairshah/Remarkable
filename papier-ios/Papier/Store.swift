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
    private static let defaultServerRoot = "http://100.90.235.68:8000"

    // The papier VM (remarkable.exe.xyz) on the tailnet; editable in Settings.
    @AppStorage("serverRoot") var serverRoot: String = LibraryStore.defaultServerRoot {
        didSet {
            guard OfflineLibraryCache.normalized(oldValue) != OfflineLibraryCache.normalized(serverRoot)
            else { return }
            refreshID += 1
            etag = nil
            docs = []
            generation = ""
            lastError = nil
            lastSuccessfulSync = nil
            restoreCachedLibrary(for: serverRoot)
        }
    }

    @Published var docs: [PapierDoc] = []
    @Published var generation: String = ""
    @Published var lastError: String?
    @Published var loading = false
    @Published private(set) var lastSuccessfulSync: Date?

    private let offlineLibrary = OfflineLibraryCache()
    private var librarySchemaVersion = 1
    private var etag: String?
    private var pollTask: Task<Void, Never>?
    private var refreshID = 0

    init() {
        let savedRoot = UserDefaults.standard.string(forKey: "serverRoot")
            ?? Self.defaultServerRoot
        restoreCachedLibrary(for: savedRoot)
    }

    var client: PapierClient {
        PapierClient(serverRoot: OfflineLibraryCache.normalized(serverRoot))
    }
    var configured: Bool { serverRoot.contains("://") }
    var isOffline: Bool { lastError != nil }

    func refresh() async {
        guard configured else { return }
        refreshID += 1
        let requestID = refreshID
        let requestRoot = OfflineLibraryCache.normalized(serverRoot)
        let requestClient = PapierClient(serverRoot: requestRoot)
        loading = docs.isEmpty
        defer {
            if requestID == refreshID { loading = false }
        }

        do {
            let (lib, newTag) = try await requestClient.library(etag: etag)
            guard requestID == refreshID,
                  requestRoot == OfflineLibraryCache.normalized(serverRoot)
            else { return }

            etag = newTag
            let syncedAt = Date()
            if let lib {
                librarySchemaVersion = lib.v
                docs = sorted(lib.docs)
                generation = lib.generation
                prunePending(docs: lib.docs)
                try? offlineLibrary.save(
                    library: lib,
                    etag: newTag,
                    serverRoot: requestRoot,
                    syncedAt: syncedAt
                )
            } else {
                let current = Library(v: librarySchemaVersion, generation: generation, docs: docs)
                try? offlineLibrary.save(
                    library: current,
                    etag: newTag,
                    serverRoot: requestRoot,
                    syncedAt: syncedAt
                )
            }
            lastSuccessfulSync = syncedAt
            lastError = nil
        } catch {
            guard requestID == refreshID else { return }
            lastError = error.localizedDescription
        }
    }

    /// Fetch current ink when online; if the server is unreachable, fall
    /// back to the version-matched copy from the last time this page opened.
    func fetchInk(_ doc: PapierDoc, key: String) async throws -> InkPage? {
        let root = OfflineLibraryCache.normalized(serverRoot)
        do {
            let page = try await PapierClient(serverRoot: root).fetchInk(doc, key: key)
            if let page {
                await OfflineInkCache.shared.save(page, serverRoot: root, doc: doc, key: key)
            }
            return page
        } catch {
            lastError = error.localizedDescription
            if let cached = await OfflineInkCache.shared.load(
                serverRoot: root,
                doc: doc,
                key: key
            ) {
                return cached
            }
            throw error
        }
    }

    private func restoreCachedLibrary(for serverRoot: String) {
        guard let snapshot = offlineLibrary.load(serverRoot: serverRoot) else { return }
        librarySchemaVersion = snapshot.library.v
        etag = snapshot.etag
        docs = sorted(snapshot.library.docs)
        generation = snapshot.library.generation
        lastSuccessfulSync = snapshot.syncedAt
        prunePending(docs: snapshot.library.docs)
    }

    private func sorted(_ docs: [PapierDoc]) -> [PapierDoc] {
        docs.sorted {
            $0.meta.title.localizedCaseInsensitiveCompare($1.meta.title) == .orderedAscending
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
