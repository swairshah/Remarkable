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

