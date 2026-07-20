import XCTest

final class EraserFlowTests: XCTestCase {
    func testRubPartiallyErasesPiInkWithoutMovingPage() throws {
        let app = XCUIApplication()
        app.launch()

        let notebook = app.staticTexts["Notebook (imported)"]
        XCTAssertTrue(notebook.waitForExistence(timeout: 15))
        notebook.tap()
        XCTAssertTrue(app.buttons["rail-goto"].waitForExistence(timeout: 15))

        // Stable fixture: imported note page 2 has one large pi patch.
        app.buttons["rail-goto"].tap()
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.typeText("2")
        app.buttons["Go"].tap()
        let counter = app.staticTexts["2 / 34"]
        XCTAssertTrue(counter.waitForExistence(timeout: 5))

        let layer = app.otherElements["pi-patch-layer"]
        XCTAssertTrue(layer.waitForExistence(timeout: 10))
        let before = layer.value as? String
        XCTAssertTrue(before?.contains("strokes") == true)

        // Simulator finger stands in for Pencil. Enabling finger drawing turns
        // the finger into ink/eraser input and disables the finger pager.
        app.buttons["rail-finger"].tap()
        app.buttons["rail-eraser"].tap()
        let start = layer.coordinate(withNormalizedOffset: CGVector(dx: 0.07, dy: 0.45))
        let end = layer.coordinate(withNormalizedOffset: CGVector(dx: 0.72, dy: 0.45))
        start.press(forDuration: 0.05, thenDragTo: end)

        let partiallyErased = NSPredicate { object, _ in
            (object as? XCUIElement)?.value as? String != before
        }
        expectation(for: partiallyErased, evaluatedWith: layer)
        waitForExpectations(timeout: 8)
        XCTAssertTrue(counter.exists, "eraser rub must not page-flip")

        try XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-smooth-erase.png"))
    }
}
