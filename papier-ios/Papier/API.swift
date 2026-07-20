// API.swift — the papier cloud client. Talks to the same nginx the web
// viewer uses (http://<vm>:8000 over the tailnet): /papier/api/* for the
// service, /papier/data|inbound/* for static document assets.

import Foundation

struct PapierAPIError: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

struct PapierClient {
    /// e.g. "http://100.101.102.103:8000" — no trailing slash.
    let serverRoot: String

    private var api: String { serverRoot + "/papier/api" }

    private static let session: URLSession = {
        let cfg = URLSessionConfiguration.default
        cfg.timeoutIntervalForRequest = 20
        cfg.requestCachePolicy = .useProtocolCachePolicy
        return URLSession(configuration: cfg)
    }()

    // MARK: reads

    /// Returns (library, etag); nil library on 304.
    func library(etag: String?) async throws -> (Library?, String?) {
        guard let url = URL(string: api + "/library") else { throw PapierAPIError(message: "bad server URL") }
        var req = URLRequest(url: url)
        if let etag { req.setValue(etag, forHTTPHeaderField: "If-None-Match") }
        let (data, resp) = try await Self.session.data(for: req)
        guard let http = resp as? HTTPURLResponse else { throw PapierAPIError(message: "no response") }
        if http.statusCode == 304 { return (nil, etag) }
        guard http.statusCode == 200 else { throw PapierAPIError(message: "library HTTP \(http.statusCode)") }
        let lib = try JSONDecoder().decode(Library.self, from: data)
        return (lib, http.value(forHTTPHeaderField: "ETag"))
    }

    func coverURL(_ doc: PapierDoc) -> URL? {
        guard let cover = doc.cover else { return nil }
        return URL(string: serverRoot + cover)
    }

    /// Pre-rendered raster for a pdf page (books), immutable per doc version.
    func pageURL(_ doc: PapierDoc, pdfPage: Int) -> URL? {
        let name = String(format: "%04d.png", pdfPage + 1)
        return URL(string: serverRoot + doc.base + "docs/\(doc.id)/pages/\(name)?v=\(doc.version)")
    }

    /// Fetch a page's ink file from the MERGED truth (inbound overlay —
    /// where iPad + cloud-pi writes land — falling back to the tablet
    /// mirror). nil when the page has no ink yet.
    func fetchInk(_ doc: PapierDoc, key: String) async throws -> InkPage? {
        guard let url = URL(string: api + "/ink?id=\(doc.id)&file=\(key).json&t=\(Date().timeIntervalSince1970)")
        else { return nil }
        var req = URLRequest(url: url)
        req.cachePolicy = .reloadIgnoringLocalCacheData
        let (data, resp) = try await Self.session.data(for: req)
        guard let http = resp as? HTTPURLResponse else { return nil }
        if http.statusCode == 404 { return nil }
        guard http.statusCode == 200 else { throw PapierAPIError(message: "ink HTTP \(http.statusCode)") }
        return InkPage.parse(data)
    }

    // MARK: writes (all land in the VM's inbound tree; the tablet pulls on wake)

    private func post(_ path: String, body: Data) async throws {
        guard let url = URL(string: api + path) else { throw PapierAPIError(message: "bad server URL") }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.httpBody = body
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let (data, resp) = try await Self.session.data(for: req)
        guard let http = resp as? HTTPURLResponse, http.statusCode == 200 else {
            let msg = String(data: data, encoding: .utf8) ?? ""
            throw PapierAPIError(message: "write failed: \(msg)")
        }
    }

    func postInk(docId: String, file: String, page: InkPage) async throws {
        try await post("/ink?id=\(docId)&file=\(file)", body: page.serialized())
    }

    func postState(docId: String, state: DocState) async throws {
        try await post("/state?id=\(docId)", body: state.serialized())
    }

    /// Erase one of pi's patches (tablet parity: the user may erase any ink).
    func erasePatch(docId: String, file: String, patchId: UInt64) async throws {
        try await post("/patch-erase?id=\(docId)&file=\(file)&patch=\(patchId)", body: Data())
    }

    /// Replace one partially-rubbed pi patch while preserving its patch id.
    func replacePatch(docId: String, file: String, patch: InkPatch,
                      nextStroke: UInt64) async throws {
        try await post("/patch-replace?id=\(docId)&file=\(file)&patch=\(patch.id)",
                       body: patch.replacementPayload(nextStroke: nextStroke))
    }

    /// Lasso-move one of pi's patches (page-unit delta).
    func movePatch(docId: String, file: String, patchId: UInt64, dx: CGFloat, dy: CGFloat) async throws {
        try await post("/patch-move?id=\(docId)&file=\(file)&patch=\(patchId)&dx=\(dx)&dy=\(dy)", body: Data())
    }

    func createNotebook(title: String) async throws -> String {
        guard let url = URL(string: api + "/notebook") else { throw PapierAPIError(message: "bad server URL") }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.httpBody = try JSONSerialization.data(withJSONObject: ["title": title])
        let (data, resp) = try await Self.session.data(for: req)
        guard let http = resp as? HTTPURLResponse, http.statusCode == 200,
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let id = obj["id"] as? String else {
            throw PapierAPIError(message: "notebook create failed")
        }
        return id
    }
}
