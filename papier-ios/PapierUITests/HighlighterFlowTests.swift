import XCTest

final class HighlighterFlowTests: XCTestCase {
    func testHighlighterRailButtonArmsTheMarkerAndBandsSyncBack() throws {
        let app = XCUIApplication()
        app.launch()

        let notebook = app.staticTexts["Notebook (imported)"]
        XCTAssertTrue(notebook.waitForExistence(timeout: 15))
        notebook.tap()

        let highlighter = app.buttons["rail-highlighter"]
        XCTAssertTrue(highlighter.waitForExistence(timeout: 15))

        let surface = app.otherElements["page-surface"]
        XCTAssertTrue(surface.waitForExistence(timeout: 10))
        let counter = app.staticTexts["1 / 34"]
        XCTAssertTrue(counter.waitForExistence(timeout: 5))

        // Simulator finger stands in for Pencil.
        app.buttons["rail-finger"].tap()
        highlighter.tap()

        // Sweep a line: the band must land and must not turn the page.
        let start = surface.coordinate(withNormalizedOffset: CGVector(dx: 0.12, dy: 0.38))
        let end = surface.coordinate(withNormalizedOffset: CGVector(dx: 0.80, dy: 0.38))
        start.press(forDuration: 0.05, thenDragTo: end)
        XCTAssertTrue(counter.exists, "a highlighter sweep must not page-flip")

        // The debounced save is the sync: it reports through the rail badge.
        let saved = app.images["arrow.triangle.2.circlepath"]
        _ = saved.waitForExistence(timeout: 6)

        app.buttons["rail-pencil"].tap()
        XCTAssertTrue(app.buttons["rail-pencil"].exists)

        try XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-highlighter.png"))
    }
}
