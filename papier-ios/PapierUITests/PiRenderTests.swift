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

        // full parity rail: lasso, goto, eraser mode cycling
        XCTAssertTrue(app.buttons["rail-lasso"].exists)
        XCTAssertTrue(app.buttons["rail-goto"].exists)
        let eraser = app.buttons["rail-eraser"]
        eraser.tap()          // select eraser (object)
        eraser.tap()          // cycle -> pixel
        eraser.tap()          // cycle -> region
        app.buttons["rail-pencil"].tap()

        // GoTo numpad jump
        app.buttons["rail-goto"].tap()
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.typeText("2")
        app.buttons["Go"].tap()
        sleep(2)
        XCTAssertTrue(app.staticTexts["2 / 3"].waitForExistence(timeout: 5), "goto lands on page 2")

        sleep(4)   // ink + patch layer render
        // deterministic artifact: the simulator shares the host filesystem
        try XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-pi-render.png"))
    }
}
