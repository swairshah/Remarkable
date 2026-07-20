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

/// The tablet's Nudge mark: one and a quarter hand-drawn sine waves.
struct PapierNudgeGlyph: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        let inset: CGFloat = 2
        let width = max(1, rect.width - inset * 2)
        let amplitude = min(5.5, rect.height * 0.25)
        for index in 0...20 {
            let t = CGFloat(index) / 20
            let point = CGPoint(
                x: rect.minX + inset + width * t,
                y: rect.midY + amplitude * sin(t * .pi * 2 * 1.25)
            )
            if index == 0 { path.move(to: point) }
            else { path.addLine(to: point) }
        }
        return path
    }
}
