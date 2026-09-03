import XCTest
@testable import Papier

final class OfflineLibraryCacheTests: XCTestCase {
    private var directory: URL!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    func testRoundTripsLibraryAndSyncMetadata() throws {
        let cache = OfflineLibraryCache(directory: directory)
        let syncedAt = Date(timeIntervalSince1970: 1_700_000_000)
        let library = makeLibrary()

        try cache.save(
            library: library,
            etag: "etag-123",
            serverRoot: " http://100.90.235.68:8000/ ",
            syncedAt: syncedAt
        )

        let restored = try XCTUnwrap(cache.load(serverRoot: "http://100.90.235.68:8000"))
        XCTAssertEqual(restored.etag, "etag-123")
        XCTAssertEqual(restored.syncedAt, syncedAt)
        XCTAssertEqual(restored.library.generation, "generation-1")
        XCTAssertEqual(restored.library.docs.first?.meta.title, "Offline Notebook")
        XCTAssertEqual(restored.library.docs.first?.seq, [.note(1), .pdf(0)])
    }

    func testDoesNotRestoreAnotherServersLibrary() throws {
        let cache = OfflineLibraryCache(directory: directory)
        try cache.save(
            library: makeLibrary(),
            etag: nil,
            serverRoot: "http://100.90.235.68:8000",
            syncedAt: Date()
        )

        XCTAssertNil(cache.load(serverRoot: "http://another-server:8000"))
    }

    func testIgnoresCorruptSnapshot() throws {
        let cache = OfflineLibraryCache(directory: directory)
        try Data("not json".utf8).write(
            to: directory.appendingPathComponent("library.json"),
            options: .atomic
        )

        XCTAssertNil(cache.load(serverRoot: "http://100.90.235.68:8000"))
    }

    func testImageIdentitySurvivesDocumentVersionBumps() throws {
        let first = try XCTUnwrap(URL(string: "http://server/papier/data/docs/id/pages/0001.png?v=one"))
        let second = try XCTUnwrap(URL(string: "http://server/papier/data/docs/id/pages/0001.png?v=two"))

        XCTAssertEqual(
            OfflineImageCache.stableIdentity(for: first),
            OfflineImageCache.stableIdentity(for: second)
        )
    }

    func testInkRemainsAvailableAfterDocumentVersionBump() async throws {
        let cache = OfflineInkCache(directory: directory)
        var page = InkPage()
        page.strokes = [
            InkStroke(id: 7, gray: 0, points: [InkPoint(x: 10, y: 20, r: 1.5)]),
        ]
        await cache.save(
            page,
            serverRoot: "http://100.90.235.68:8000",
            doc: makeDoc(version: "version-1"),
            key: "note-0001"
        )

        let restored = await cache.load(
            serverRoot: "http://100.90.235.68:8000",
            doc: makeDoc(version: "version-2"),
            key: "note-0001"
        )
        XCTAssertEqual(restored?.strokes.first?.id, 7)
        XCTAssertEqual(restored?.strokes.first?.points.first?.x, 10)
    }

    func testLibrarySortSupportsUpdatedAddedAndTitle() {
        let olderAddition = makeDoc(
            id: "alpha",
            title: "Alpha",
            addedAt: 1_700_000_000_000,
            mtime: 1_900_000_000_000
        )
        let newerAddition = makeDoc(
            id: "zulu",
            title: "Zulu",
            addedAt: 1_800_000_000_000,
            mtime: 1_800_000_000_000
        )

        XCTAssertEqual(LibrarySort.recent.sorted([olderAddition, newerAddition]).map(\.id), ["alpha", "zulu"])
        XCTAssertEqual(LibrarySort.added.sorted([olderAddition, newerAddition]).map(\.id), ["zulu", "alpha"])
        XCTAssertEqual(LibrarySort.title.sorted([olderAddition, newerAddition]).map(\.id), ["alpha", "zulu"])
        XCTAssertEqual(newerAddition.addedDate?.timeIntervalSince1970, 1_800_000_000)
        XCTAssertEqual(newerAddition.modifiedAt?.timeIntervalSince1970, 1_800_000_000)
    }

    private func makeLibrary() -> Library {
        Library(v: 1, generation: "generation-1", docs: [makeDoc()])
    }

    private func makeDoc(
        id: String = "offline-notebook",
        title: String = "Offline Notebook",
        addedAt: Double = 1_600_000_000_000,
        mtime: Double = 1_700_000_000_000,
        version: String = "version-1"
    ) -> PapierDoc {
        PapierDoc(
            id: id,
            base: "/papier/inbound/",
            pending: false,
            addedAt: addedAt,
            mtime: mtime,
            meta: DocMeta(
                title: title,
                pages: 2,
                w: 1404,
                h: 1872,
                kind: "notebook",
                folder: nil
            ),
            version: version,
            cover: "/papier/api/cover?id=offline-notebook&v=\(version)",
            seq: [.note(1), .pdf(0)],
            ink: ["note-0001"]
        )
    }
}
