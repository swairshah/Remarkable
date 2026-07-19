// PapierApp.swift — Papier for iPad: the same documents, the same cloud,
// pencil in hand. Talks to the papier VM (remarkable.exe.xyz) over the
// tailnet; edits land in the inbound tree and flow to the reMarkable on
// its next wake.

import SwiftUI

@main
struct PapierApp: App {
    @StateObject private var store = LibraryStore()

    var body: some Scene {
        WindowGroup {
            HomeView()
                .environmentObject(store)
        }
    }
}
