// Models.swift — the papier cloud data model, mirroring the library manifest
// (papier-library.js) and the libreink-page ink JSON schema byte-for-byte.

import Foundation

// MARK: - Library manifest (/papier/api/library)

struct Library: Decodable {
    let v: Int
    let generation: String
    let docs: [PapierDoc]
}

struct PapierDoc: Decodable, Identifiable, Equatable, Hashable {
    let id: String
    let base: String            // "/papier/data/" (mirror) or "/papier/inbound/"
    let pending: Bool           // true = web/iPad-added, not yet pulled by the tablet
    let meta: DocMeta
    let version: String         // bumps whenever any doc file changes
    let cover: String?          // absolute path: /papier/api/cover?...
    let seq: [SeqEntry]         // page sequence: pdf pages and note pages interleaved
    let ink: [String]?          // existing ink keys, e.g. ["pdf-0002", "note-0001"]

    static func == (a: PapierDoc, b: PapierDoc) -> Bool {
        a.id == b.id && a.version == b.version && a.pending == b.pending
    }

    func hash(into hasher: inout Hasher) {
        hasher.combine(id)
        hasher.combine(version)
    }

    var isNotebook: Bool { meta.kind == "notebook" }
    var pageW: Double { meta.w ?? 1404 }
    var pageH: Double { meta.h ?? 1872 }
}

struct DocMeta: Decodable {
    let title: String
    let pages: Int?
    let w: Double?
    let h: Double?
    let kind: String?
    let folder: String?
}

/// One entry of a document's page sequence: {"p": N} (0-based pdf page)
/// or {"n": N} (1-based note page).
enum SeqEntry: Decodable, Hashable {
    case pdf(Int)
    case note(Int)

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let p = try c.decodeIfPresent(Int.self, forKey: .p) { self = .pdf(p); return }
        if let n = try c.decodeIfPresent(Int.self, forKey: .n) { self = .note(n); return }
        throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath,
                                                debugDescription: "seq entry has neither p nor n"))
    }

    private enum CodingKeys: String, CodingKey { case p, n }

    /// The ink file stem, matching papier-library.js inkKey().
    var inkKey: String {
        switch self {
        case .pdf(let p): return String(format: "pdf-%04d", p + 1)
        case .note(let n): return String(format: "note-%04d", n)
        }
    }

    var jsonObject: [String: Int] {
        switch self {
        case .pdf(let p): return ["p": p]
        case .note(let n): return ["n": n]
        }
    }
}

// MARK: - Ink page file (libreink-page schema)

/// A point of a stroke, page coordinates (1404x1872 space). Serialized as
/// flat [x*10, y*10, r*10, ...] integer triplets.
struct InkPoint {
    var x: Double
    var y: Double
    var r: Double   // half-width of the mark at this point
}

struct InkStroke {
    var id: UInt64
    var gray: Int          // 0 = black
    var points: [InkPoint]
}

struct InkTextRun {
    var x: Double
    var y: Double          // baseline
    var size: Double
    var gray: Int
    var text: String
}

/// pi's ink lives in patches; the user's in top-level strokes.
struct InkPatch {
    var id: UInt64
    var strokes: [InkStroke]
    var texts: [InkTextRun]
}

struct InkPage {
    var nextPatch: UInt64 = 1
    var nextStroke: UInt64 = 1
    var strokes: [InkStroke] = []
    var patches: [InkPatch] = []

    // -- JSON (hand-rolled: the schema is tiny and exact) ------------------

    static func parse(_ data: Data) -> InkPage? {
        guard let v = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              v["v"] as? Int == 1 else { return nil }
        var page = InkPage()
        page.nextPatch = (v["next_patch"] as? UInt64) ?? UInt64((v["next_patch"] as? Int) ?? 1)
        page.nextStroke = (v["next_stroke"] as? UInt64) ?? UInt64((v["next_stroke"] as? Int) ?? 1)
        page.strokes = (v["strokes"] as? [[String: Any]])?.compactMap(Self.stroke(from:)) ?? []
        page.patches = (v["patches"] as? [[String: Any]])?.compactMap { p in
            guard let id = Self.uint(p["id"]) else { return nil }
            return InkPatch(
                id: id,
                strokes: (p["strokes"] as? [[String: Any]])?.compactMap(Self.stroke(from:)) ?? [],
                texts: (p["texts"] as? [[String: Any]])?.compactMap(Self.text(from:)) ?? [])
        } ?? []
        return page
    }

    private static func uint(_ v: Any?) -> UInt64? {
        if let u = v as? UInt64 { return u }
        if let i = v as? Int, i >= 0 { return UInt64(i) }
        if let d = v as? Double, d >= 0 { return UInt64(d) }
        return nil
    }

    private static func stroke(from v: [String: Any]) -> InkStroke? {
        guard let flat = v["p"] as? [Any] else { return nil }
        var pts: [InkPoint] = []
        pts.reserveCapacity(flat.count / 3)
        var i = 0
        while i + 2 < flat.count {
            guard let x = flat[i] as? NSNumber, let y = flat[i + 1] as? NSNumber,
                  let r = flat[i + 2] as? NSNumber else { return nil }
            pts.append(InkPoint(x: x.doubleValue / 10, y: y.doubleValue / 10, r: r.doubleValue / 10))
            i += 3
        }
        return InkStroke(id: uint(v["i"]) ?? 0, gray: (v["g"] as? Int) ?? 0, points: pts)
    }

    private static func text(from v: [String: Any]) -> InkTextRun? {
        guard let t = v["t"] as? String, let x = v["x"] as? NSNumber, let y = v["y"] as? NSNumber
        else { return nil }
        let s = (v["s"] as? NSNumber)?.doubleValue ?? 400
        return InkTextRun(x: x.doubleValue / 10, y: y.doubleValue / 10,
                          size: s > 0 ? s / 10 : 40, gray: (v["g"] as? Int) ?? 0, text: t)
    }

    func serialized() -> Data {
        func strokeJson(_ s: InkStroke) -> [String: Any] {
            var flat: [Int] = []
            flat.reserveCapacity(s.points.count * 3)
            for p in s.points {
                flat.append(Int((p.x * 10).rounded()))
                flat.append(Int((p.y * 10).rounded()))
                flat.append(Int((p.r * 10).rounded()))
            }
            return ["i": s.id, "g": s.gray, "p": flat]
        }
        let doc: [String: Any] = [
            "v": 1,
            "next_patch": nextPatch,
            "next_stroke": nextStroke,
            "strokes": strokes.map(strokeJson),
            "patches": patches.map { p in
                ["id": p.id,
                 "strokes": p.strokes.map(strokeJson),
                 "texts": p.texts.map { t in
                     ["x": Int((t.x * 10).rounded()), "y": Int((t.y * 10).rounded()),
                      "s": Int((t.size * 10).rounded()), "g": t.gray, "t": t.text] as [String: Any]
                 }] as [String: Any]
            },
        ]
        return (try? JSONSerialization.data(withJSONObject: doc)) ?? Data("{}".utf8)
    }
}

// MARK: - state.json

struct DocState {
    var nextNote: Int
    var pos: Int
    var seq: [SeqEntry]

    func serialized() -> Data {
        let doc: [String: Any] = [
            "next_note": nextNote,
            "pos": pos,
            "seq": seq.map { $0.jsonObject },
        ]
        return (try? JSONSerialization.data(withJSONObject: doc)) ?? Data("{}".utf8)
    }
}
