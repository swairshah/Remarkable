import XCTest

final class PagerFlowTests: XCTestCase {
    func testForwardThenEdgeBackSwipeChangesPageWithoutClosingDocument() {
        let app = XCUIApplication()
        app.launch()

        let notebook = app.staticTexts["Notebook (imported)"]
        XCTAssertTrue(notebook.waitForExistence(timeout: 15))
        notebook.tap()
        XCTAssertTrue(app.buttons["rail-goto"].waitForExistence(timeout: 15))

        // Begin from a known page regardless of the user's saved position.
        app.buttons["rail-goto"].tap()
        let field = app.textFields.firstMatch
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.typeText("1")
        app.buttons["Go"].tap()
        XCTAssertTrue(app.staticTexts["1 / 34"].waitForExistence(timeout: 5))

        let page = app.otherElements["page-surface"].firstMatch
        XCTAssertTrue(page.waitForExistence(timeout: 10))

        // Finger left: next page.
        page.coordinate(withNormalizedOffset: CGVector(dx: 0.78, dy: 0.5))
            .press(forDuration: 0.05,
                   thenDragTo: page.coordinate(withNormalizedOffset: CGVector(dx: 0.18, dy: 0.5)))
        XCTAssertTrue(app.staticTexts["2 / 34"].waitForExistence(timeout: 5))

        // Begin the reverse swipe at the physical left edge—the exact gesture
        // that previously leaked into NavigationStack and closed the document.
        let currentPage = app.otherElements["page-surface"].firstMatch
        currentPage.coordinate(withNormalizedOffset: CGVector(dx: 0.01, dy: 0.5))
            .press(forDuration: 0.05,
                   thenDragTo: currentPage.coordinate(withNormalizedOffset: CGVector(dx: 0.72, dy: 0.5)))

        XCTAssertTrue(app.staticTexts["1 / 34"].waitForExistence(timeout: 5))
        XCTAssertTrue(app.buttons["rail-goto"].exists,
                      "reverse page swipe must not dismiss the document")

        try? XCUIScreen.main.screenshot().pngRepresentation
            .write(to: URL(fileURLWithPath: "/tmp/papier-pager-reverse.png"))
    }
}
