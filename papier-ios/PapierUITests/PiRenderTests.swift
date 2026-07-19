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
        field.typeText("5")
        app.buttons["Go"].tap()
        let pageCounter = app.staticTexts.matching(
            NSPredicate(format: "label BEGINSWITH '5 / '")
        ).firstMatch
        XCTAssertTrue(pageCounter.waitForExistence(timeout: 5), "goto lands on page 5")

        // Outcome assertion, not transport assertion: pi's actual patches
        // must be attached to the visible page model and render onscreen.
        let layer = app.otherElements["pi-patch-layer"]
        XCTAssertTrue(layer.waitForExistence(timeout: 10), "pi patch layer rendered")
        let before = layer.value as? String
        XCTAssertNotEqual(before, "0", "visible page contains pi ink")

        // Rub across pi's first line. This is a PAN, not a tap — the precise
        // failure mode of Apple Pencil erasing in the old build.
        eraser.tap() // reselect (still region from mode-cycle above)
        eraser.tap() // cycle region -> object
        // Coordinates relative to the paper/patch layer: patch #1 spans
        // x=218...698, y=668...719 in a 1404x1872 page.
        let start = layer.coordinate(withNormalizedOffset: CGVector(dx: 0.16, dy: 0.37))
        let end = layer.coordinate(withNormalizedOffset: CGVector(dx: 0.50, dy: 0.37))
        start.press(forDuration: 0.05, thenDragTo: end)
        let erased = NSPredicate { object, _ in
            (object as? XCUIElement)?.value as? String != before
        }
        expectation(for: erased, evaluatedWith: layer)
        waitForExpectations(timeout: 8)

        sleep(1)   // update settles before artifact
        // deterministic artifact: the simulator shares the host filesystem
        try XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-pi-render.png"))
    }
}
