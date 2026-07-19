// PaperTone.swift — the page's paper color. Real paper isn't #FFFFFF:
// the default is a warm off-white, and every tone dims further in dark
// mode so night reading is easy on the eyes. Ink stays true black; book
// rasters are tinted by multiplication (white paper -> tone, print stays
// dark), so books and notebooks share the same paper.

import SwiftUI

enum PaperTone: String, CaseIterable, Identifiable {
    case white, paper, soft

    var id: String { rawValue }

    var label: String {
        switch self {
        case .white: return "White"
        case .paper: return "Paper"
        case .soft: return "Soft"
        }
    }

    func color(dark: Bool) -> Color {
        switch (self, dark) {
        case (.white, false): return Color(red: 1.00, green: 1.00, blue: 1.00)
        case (.white, true):  return Color(red: 0.914, green: 0.914, blue: 0.914)
        case (.paper, false): return Color(red: 0.980, green: 0.973, blue: 0.949)
        case (.paper, true):  return Color(red: 0.890, green: 0.878, blue: 0.847)
        case (.soft, false):  return Color(red: 0.937, green: 0.929, blue: 0.902)
        case (.soft, true):   return Color(red: 0.839, green: 0.824, blue: 0.788)
        }
    }

    static var current: PaperTone {
        PaperTone(rawValue: UserDefaults.standard.string(forKey: "paperTone") ?? "") ?? .paper
    }
}
