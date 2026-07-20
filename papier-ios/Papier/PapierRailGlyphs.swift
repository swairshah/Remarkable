// PapierRailGlyphs.swift — the exact compact Pi/Nudge geometry used by the
// reMarkable toolbar, redrawn as resolution-independent SwiftUI shapes.

import SwiftUI

/// The tablet's pixel Pi: a blocky P with an i tucked into its lower-right.
struct PapierPiGlyph: Shape {
    private static let rows: [[Bool]] = [
        [true,  true,  true,  false],
        [true,  false, true,  false],
        [true,  true,  false, true ],
        [true,  false, false, true ],
        [true,  false, false, true ],
    ]

    func path(in rect: CGRect) -> Path {
        let cell = floor(min(rect.width / 4, rect.height / 5))
        let width = cell * 4
        let height = cell * 5
        let x0 = rect.midX - width / 2
        let y0 = rect.midY - height / 2
        var path = Path()
        for (rowIndex, row) in Self.rows.enumerated() {
            for (columnIndex, filled) in row.enumerated() where filled {
                path.addRect(CGRect(x: x0 + CGFloat(columnIndex) * cell,
                                    y: y0 + CGFloat(rowIndex) * cell,
                                    width: cell, height: cell))
            }
        }
        return path
    }
}

/// Nudge as an action, not an abstract sparkle: a small fingertip presses
/// directly into the tablet's block Pi. Filled geometry keeps it legible at
/// the rail's tiny display size.
struct PapierNudgeGlyph: Shape {
    private static let piRows: [[Bool]] = [
        [true,  true,  true,  false],
        [true,  false, true,  false],
        [true,  true,  false, true ],
        [true,  false, false, true ],
        [true,  false, false, true ],
    ]

    func path(in rect: CGRect) -> Path {
        var path = Path()
        let unit = min(rect.width / 30, rect.height / 24)
        let x = rect.midX - 15 * unit
        let y = rect.midY - 12 * unit

        // Pointing finger: rounded fingertip touches the Pi; the small lower
        // contour reads as a bent hand rather than an arrow.
        path.addRoundedRect(in: CGRect(x: x + 1 * unit, y: y + 10 * unit,
                                       width: 15 * unit, height: 4 * unit),
                            cornerSize: CGSize(width: 2 * unit, height: 2 * unit))
        var hand = Path()
        hand.move(to: CGPoint(x: x + 2 * unit, y: y + 14 * unit))
        hand.addLine(to: CGPoint(x: x + 6 * unit, y: y + 18 * unit))
        hand.addLine(to: CGPoint(x: x + 11 * unit, y: y + 18 * unit))
        hand.addLine(to: CGPoint(x: x + 14 * unit, y: y + 14 * unit))
        hand.addLine(to: CGPoint(x: x + 10 * unit, y: y + 14 * unit))
        hand.addLine(to: CGPoint(x: x + 8 * unit, y: y + 16 * unit))
        hand.addLine(to: CGPoint(x: x + 5 * unit, y: y + 14 * unit))
        hand.closeSubpath()
        path.addPath(hand)

        // Miniature Pi, with a one-unit air gap at the point of contact.
        let cell = 2.6 * unit
        let piX = x + 17 * unit
        let piY = y + 5.5 * unit
        for (rowIndex, row) in Self.piRows.enumerated() {
            for (columnIndex, filled) in row.enumerated() where filled {
                path.addRect(CGRect(x: piX + CGFloat(columnIndex) * cell,
                                    y: piY + CGFloat(rowIndex) * cell,
                                    width: cell, height: cell))
            }
        }
        return path
    }
}
