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
        field.typeText("4")
        app.buttons["Go"].tap()
        XCTAssertTrue(app.staticTexts["4 / 4"].waitForExistence(timeout: 5), "goto lands on page 4")

        // Outcome assertion, not transport assertion: pi's actual patches
        // must be attached to the visible page model and render onscreen.
        let layer = app.otherElements["pi-patch-layer"]
        XCTAssertTrue(layer.waitForExistence(timeout: 10), "pi patch layer rendered")
        let before = layer.value as? String
        XCTAssertNotEqual(before, "0", "visible page contains pi ink")

        // End-to-end regression for the bug the user saw: nudge a real
        // resident pi and require the VISIBLE layer's count to increase.
        app.buttons["rail-nudge"].tap()
        let changed = NSPredicate { object, _ in
            (object as? XCUIElement)?.value as? String != before
        }
        expectation(for: changed, evaluatedWith: layer)
        waitForExpectations(timeout: 40)

        sleep(2)   // animation settles before artifact
        // deterministic artifact: the simulator shares the host filesystem
        try XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-pi-render.png"))
    }
}
