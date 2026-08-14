import CryptoKit
import Foundation

struct OfflineLibrarySnapshot: Codable {
    let serverRoot: String
    let etag: String?
    let syncedAt: Date
    let library: Library
}

/// Durable, server-scoped storage for the last library manifest. Application
/// Support is intentional: unlike URLCache/Caches, iOS does not routinely
/// purge this file when the device needs space.
struct OfflineLibraryCache {
    private let fileURL: URL

    init(directory: URL? = nil) {
        let root = directory ?? FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        )[0].appendingPathComponent("Papier/Offline", isDirectory: true)
        fileURL = root.appendingPathComponent("library.json")
    }

    func load(serverRoot: String) -> OfflineLibrarySnapshot? {
        guard let data = try? Data(contentsOf: fileURL),
              let snapshot = try? JSONDecoder().decode(OfflineLibrarySnapshot.self, from: data),
              Self.normalized(snapshot.serverRoot) == Self.normalized(serverRoot)
        else { return nil }
        return snapshot
    }

    func save(library: Library, etag: String?, serverRoot: String, syncedAt: Date) throws {
        let directory = fileURL.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutableDirectory = directory
        try? mutableDirectory.setResourceValues(values)

        let snapshot = OfflineLibrarySnapshot(
            serverRoot: Self.normalized(serverRoot),
            etag: etag,
            syncedAt: syncedAt,
            library: library
        )
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        try encoder.encode(snapshot).write(to: fileURL, options: .atomic)
    }

    static func normalized(_ value: String) -> String {
        var result = value.trimmingCharacters(in: .whitespacesAndNewlines)
        while result.hasSuffix("/") { result.removeLast() }
        return result
    }
}

private enum OfflineCacheError: LocalizedError {
    case invalidResponse

    var errorDescription: String? { "The server returned an invalid image response." }
}

/// Persistent, bounded image storage for covers and previously viewed PDF
/// pages. A stable identity ignores the `v` cache-buster so the last render
/// remains available after an ink edit bumps the document-wide version.
actor OfflineImageCache {
    static let shared = OfflineImageCache()

    struct Hit {
        let data: Data
        let isCurrentVersion: Bool
    }

    private let directory: URL
    private let session: URLSession
    private let maximumBytes = 384 * 1024 * 1024

    init(directory: URL? = nil, session: URLSession? = nil) {
        self.directory = directory ?? FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        )[0].appendingPathComponent("Papier/Offline/Images", isDirectory: true)

        if let session {
            self.session = session
        } else {
            let configuration = URLSessionConfiguration.default
            configuration.timeoutIntervalForRequest = 20
            configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
            self.session = URLSession(configuration: configuration)
        }
    }

    func cachedData(for url: URL) -> Hit? {
        let paths = fileURLs(for: url)
        guard let data = try? Data(contentsOf: paths.image) else { return nil }
        let savedURL = try? String(contentsOf: paths.source, encoding: .utf8)
        try? FileManager.default.setAttributes(
            [.modificationDate: Date()],
            ofItemAtPath: paths.image.path
        )
        return Hit(data: data, isCurrentVersion: savedURL == url.absoluteString)
    }

    func fetchAndStore(_ url: URL) async throws -> Data {
        let (data, response) = try await session.data(from: url)
        guard let http = response as? HTTPURLResponse,
              (200..<300).contains(http.statusCode),
              !data.isEmpty
        else { throw OfflineCacheError.invalidResponse }

        try prepareDirectory()
        let paths = fileURLs(for: url)
        try data.write(to: paths.image, options: .atomic)
        try Data(url.absoluteString.utf8).write(to: paths.source, options: .atomic)
        trimIfNeeded()
        return data
    }

    static func stableIdentity(for url: URL) -> String {
        guard var components = URLComponents(url: url, resolvingAgainstBaseURL: false) else {
            return url.absoluteString
        }
        components.queryItems = components.queryItems?.filter { $0.name != "v" }
        if components.queryItems?.isEmpty == true { components.queryItems = nil }
        return components.url?.absoluteString ?? url.absoluteString
    }

    private func fileURLs(for url: URL) -> (image: URL, source: URL) {
        let key = Self.stableIdentity(for: url)
        let digest = SHA256.hash(data: Data(key.utf8)).map { String(format: "%02x", $0) }.joined()
        return (
            directory.appendingPathComponent(digest + ".image"),
            directory.appendingPathComponent(digest + ".url")
        )
    }

    private func prepareDirectory() throws {
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutableDirectory = directory
        try? mutableDirectory.setResourceValues(values)
    }

    private func trimIfNeeded() {
        let keys: Set<URLResourceKey> = [.isRegularFileKey, .fileSizeKey, .contentModificationDateKey]
        guard let files = try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: Array(keys),
            options: .skipsHiddenFiles
        ) else { return }

        var entries: [(url: URL, bytes: Int, modified: Date)] = files.compactMap { url in
            guard url.pathExtension == "image",
                  let values = try? url.resourceValues(forKeys: keys),
                  values.isRegularFile == true
            else { return nil }
            return (url, values.fileSize ?? 0, values.contentModificationDate ?? .distantPast)
        }
        var total = entries.reduce(0) { $0 + $1.bytes }
        guard total > maximumBytes else { return }

        entries.sort { $0.modified < $1.modified }
        for entry in entries where total > maximumBytes {
            if (try? FileManager.default.removeItem(at: entry.url)) != nil {
                try? FileManager.default.removeItem(
                    at: entry.url.deletingPathExtension().appendingPathExtension("url")
                )
                total -= entry.bytes
            }
        }
    }
}

/// Last-known ink for pages the user has opened. The key deliberately ignores
/// the document-wide version so an edit to another page does not blank this one offline.
actor OfflineInkCache {
    static let shared = OfflineInkCache()

    private let directory: URL

    init(directory: URL? = nil) {
        self.directory = directory ?? FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        )[0].appendingPathComponent("Papier/Offline/Ink", isDirectory: true)
    }

    func load(serverRoot: String, doc: PapierDoc, key: String) -> InkPage? {
        guard let data = try? Data(contentsOf: fileURL(
            serverRoot: serverRoot,
            doc: doc,
            key: key
        )) else { return nil }
        return InkPage.parse(data)
    }

    func save(_ page: InkPage, serverRoot: String, doc: PapierDoc, key: String) {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            var mutableDirectory = directory
            try? mutableDirectory.setResourceValues(values)
            try page.serialized().write(
                to: fileURL(serverRoot: serverRoot, doc: doc, key: key),
                options: .atomic
            )
        } catch {
            // A cache write must never turn a successful server read into a failure.
        }
    }

    private func fileURL(serverRoot: String, doc: PapierDoc, key: String) -> URL {
        let identity = [
            OfflineLibraryCache.normalized(serverRoot),
            doc.id,
            key,
        ].joined(separator: "|")
        let digest = SHA256.hash(data: Data(identity.utf8)).map { String(format: "%02x", $0) }.joined()
        return directory.appendingPathComponent(digest + ".json")
    }
}
