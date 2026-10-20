import PencilKit
import XCTest
@testable import Papier

/// A highlight is an ordinary papier stroke with a pale `gray` and a fat
/// radius (the tablet's src/highlight.rs). These pin the two directions of
/// that mapping so a band drawn on either device survives the round trip.
final class HighlighterBridgeTests: XCTestCase {
    private let scale: CGFloat = 0.75

    func testHighlighterToolIsATranslucentMarker() {
        let tool = PencilBridge.highlighterTool()
        XCTAssertEqual(tool.inkType, .marker)
        // PencilKit clamps a tool's width to its ink's own range; all that
        // matters is that the band lands far fatter than the pen nib.
        XCTAssertGreaterThan(tool.width, PencilBridge.pencilTool().width * 3)
    }

    func testHighlightGreyIsClearOfInkAndPiInk() {
        XCTAssertTrue(PencilBridge.isHighlight(PencilBridge.highlightGray))
        XCTAssertFalse(PencilBridge.isHighlight(0))    // the user's ink
        XCTAssertFalse(PencilBridge.isHighlight(110))  // pi's mid-grey
    }

    func testABandComesBackAsMarkerInkSoItDoesNotCoverTheWords() {
        var page = InkPage()
        page.strokes = [band(), ink()]
        let drawing = PencilBridge.drawing(from: page, scale: scale)

        XCTAssertEqual(drawing.strokes.count, 2)
        XCTAssertEqual(drawing.strokes[0].ink.inkType, .marker)
        XCTAssertEqual(drawing.strokes[1].ink.inkType, .monoline)
    }

    func testAMarkerStrokeSerializesAsAHighlightAndPencilStaysInk() throws {
        let marker = PKStroke(ink: PKInk(.marker, color: PencilBridge.highlightColor),
                              path: path(width: 23 * 2 * scale))
        let pencil = PKStroke(ink: PKInk(.pencil, color: PencilBridge.inkBlack),
                              path: path(width: 4.5))

        let out = PencilBridge.inkStrokes(from: PKDrawing(strokes: [marker, pencil]),
                                          scale: scale, firstId: 1)
        XCTAssertEqual(out.count, 2)
        XCTAssertEqual(out[0].gray, PencilBridge.highlightGray)
        XCTAssertEqual(out[1].gray, 0)

        // the band keeps its real half-height; ink keeps the pen's range
        let bandR = try XCTUnwrap(out[0].points.first?.r)
        XCTAssertEqual(bandR, 23, accuracy: 2)
        let inkR = try XCTUnwrap(out[1].points.first?.r)
        XCTAssertLessThan(inkR, 4)
    }

    // MARK: - fixtures

    private func band() -> InkStroke {
        InkStroke(id: 1, gray: PencilBridge.highlightGray,
                  points: (0...20).map { InkPoint(x: 200 + Double($0) * 20, y: 600, r: 23) })
    }

    private func ink() -> InkStroke {
        InkStroke(id: 2, gray: 0,
                  points: (0...20).map { InkPoint(x: 200 + Double($0) * 20, y: 600, r: 2.4) })
    }

    private func path(width: CGFloat) -> PKStrokePath {
        let points = (0...20).map { i in
            PKStrokePoint(location: CGPoint(x: 150 + CGFloat(i) * 15, y: 450),
                          timeOffset: TimeInterval(i) * 0.008,
                          size: CGSize(width: width, height: width),
                          opacity: 1, force: 1, azimuth: 0, altitude: .pi / 2)
        }
        return PKStrokePath(controlPoints: points, creationDate: Date())
    }
}
