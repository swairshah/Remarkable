import XCTest

final class RailVisualTests: XCTestCase {
    func testTabletPiGlyphsAndBusyDot() throws {
        let app = XCUIApplication()
        app.launchEnvironment["PAPIER_UI_TEST_FORCE_BUSY"] = "1"
        app.launch()

        let notebook = app.staticTexts["Notebook (imported)"]
        XCTAssertTrue(notebook.waitForExistence(timeout: 15))
        notebook.tap()

        XCTAssertTrue(app.buttons["rail-pimode"].waitForExistence(timeout: 15))
        XCTAssertTrue(app.buttons["rail-nudge"].exists)
        XCTAssertTrue(app.otherElements["rail-busy-dot"].exists)
        try XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-tablet-rail.png"))
    }
}
