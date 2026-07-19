// PiRenderTests.swift — opens the doc where cloud pi drew, holds it on
// screen (the harness screenshots the simulator during the hold), and
// checks the pi rail controls exist.

import XCTest

final class PiRenderTests: XCTestCase {
    func testOpenPiAnnotatedDoc() throws {
        let app = XCUIApplication()
        app.launch()

        let cell = app.staticTexts["iPad Sync Test"]
        XCTAssertTrue(cell.waitForExistence(timeout: 15))
        cell.tap()

        XCTAssertTrue(app.buttons["rail-nudge"].waitForExistence(timeout: 15), "pi rail present")
        XCTAssertTrue(app.buttons["rail-pimode"].exists)
        XCTAssertTrue(app.buttons["rail-pifont"].exists)
        sleep(6)   // ink + patch layer render
        // deterministic artifact: the simulator shares the host filesystem
        try XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-pi-render.png"))
    }
}
