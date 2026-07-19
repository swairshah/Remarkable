// SyncFlowTests.swift — end-to-end against the real papier cloud (via the
// dev tunnel): open the imported notebook, enable finger drawing, draw a
// stroke, and give the debounced save time to POST. The harness then
// checks the VM's inbound tree for the written ink file.

import XCTest

final class SyncFlowTests: XCTestCase {
    func testOpenNotebookDrawAndSync() throws {
        let app = XCUIApplication()
        app.launch()

        // home grid -> the imported notebook
        let cell = app.staticTexts["Notebook (imported)"]
        XCTAssertTrue(cell.waitForExistence(timeout: 15), "library should load the notebook")
        cell.tap()

        // wait for the tool rail (document open)
        let finger = app.buttons["rail-finger"]
        XCTAssertTrue(finger.waitForExistence(timeout: 15), "document should open")
        sleep(3)   // ink load
        finger.tap()   // allow finger drawing

        // draw a diagonal squiggle mid-page
        let window = app.windows.firstMatch
        let start = window.coordinate(withNormalizedOffset: CGVector(dx: 0.35, dy: 0.4))
        let mid = window.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5))
        let end = window.coordinate(withNormalizedOffset: CGVector(dx: 0.55, dy: 0.42))
        start.press(forDuration: 0.05, thenDragTo: mid, withVelocity: .slow, thenHoldForDuration: 0.05)
        mid.press(forDuration: 0.05, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 0.05)

        // debounced save (2.5s) + POST
        sleep(7)

        // the sync badge should show saved (checkmark.icloud) not error
        XCTAssertFalse(app.images["exclamationmark.icloud"].exists, "sync must not error")
    }
}
