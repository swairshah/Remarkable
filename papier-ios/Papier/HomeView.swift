// HomeView.swift — papier's xochitl-like home grid: covers and titles.
// Inbound-only documents are already usable cloud documents; they are not
// presented as perpetually "syncing" while the tablet happens to be asleep.

import SwiftUI

struct HomeView: View {
    @EnvironmentObject private var store: LibraryStore
    @State private var showSettings = false
    @State private var newNotebookTitle = ""
    @State private var askNotebookTitle = false
    @State private var openedDoc: PapierDoc?

    private let columns = [GridItem(.adaptive(minimum: 168, maximum: 230), spacing: 22)]

    var body: some View {
        NavigationStack {
            Group {
                if !store.configured {
                    unconfigured
                } else if store.docs.isEmpty && store.loading {
                    ProgressView("Loading library…")
                } else if store.docs.isEmpty && store.isOffline {
                    offlineEmptyState
                } else if store.docs.isEmpty {
                    emptyState
                } else {
                    grid
                }
            }
            .navigationTitle("Papier")
            .toolbar {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    if let err = store.lastError {
                        Image(systemName: "wifi.exclamationmark")
                            .foregroundStyle(.red)
                            .help(err)
                    }
                    Button { askNotebookTitle = true } label: {
                        Label("New Notebook", systemImage: "square.and.pencil")
                    }
                    Button { showSettings = true } label: {
                        Label("Settings", systemImage: "gearshape")
                    }
                }
            }
            // Full-screen, NOT pushed: a pushed document inherits the
            // navigation pop gesture, which raced page-back swipes and
            // closed the document. A cover has no system dismiss gesture.
            .fullScreenCover(item: $openedDoc) { doc in
                NavigationStack {
                    DocumentView(doc: doc)
                }
                .environmentObject(store)
            }
            .sheet(isPresented: $showSettings) { SettingsView().environmentObject(store) }
            .alert("New Notebook", isPresented: $askNotebookTitle) {
                TextField("Title", text: $newNotebookTitle)
                Button("Create") { createNotebook() }
                Button("Cancel", role: .cancel) {}
            } message: {
                Text("Appears here immediately and on the reMarkable after its next sync.")
            }
            .refreshable { await store.refresh() }
            .task {
                await store.refresh()
                store.startPolling()
            }
        }
    }

    private var grid: some View {
        ScrollView {
            VStack(spacing: 18) {
                if store.isOffline { offlineBanner }
                LazyVGrid(columns: columns, spacing: 26) {
                    ForEach(store.docs) { doc in
                        Button { openedDoc = doc } label: { DocCell(doc: doc) }
                            .buttonStyle(.plain)
                    }
                }
            }
            .padding(22)
        }
        .background(Color(uiColor: .systemGroupedBackground))
    }

    private var offlineBanner: some View {
        HStack(spacing: 12) {
            Image(systemName: "icloud.slash")
                .font(.system(size: 17, weight: .semibold))
                .foregroundStyle(.orange)
                .frame(width: 28, height: 28)
            VStack(alignment: .leading, spacing: 2) {
                Text("Offline — showing saved library")
                    .font(.subheadline.weight(.semibold))
                if let date = store.lastSuccessfulSync {
                    Text("Last synced \(date.formatted(.relative(presentation: .named)))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            Spacer(minLength: 12)
            Button {
                Task { await store.refresh() }
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 40, height: 40)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Retry connection")
        }
        .padding(.leading, 14)
        .padding(.trailing, 6)
        .padding(.vertical, 8)
        .background(Color.orange.opacity(0.10), in: RoundedRectangle(cornerRadius: 14))
        .overlay {
            RoundedRectangle(cornerRadius: 14)
                .stroke(Color.orange.opacity(0.18), lineWidth: 1)
        }
        .frame(maxWidth: .infinity)
    }

    private var unconfigured: some View {
        ContentUnavailableView {
            Label("Not connected", systemImage: "server.rack")
        } description: {
            Text("Point Papier at your reMarkable cloud (the VM's tailnet address).")
        } actions: {
            Button("Open Settings") { showSettings = true }.buttonStyle(.borderedProminent)
        }
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label("No documents", systemImage: "books.vertical")
        } description: {
            Text("Documents added on the tablet or dropped on the web viewer appear here.")
        }
    }

    private var offlineEmptyState: some View {
        ContentUnavailableView {
            Label("Library unavailable", systemImage: "icloud.slash")
        } description: {
            Text("Papier cannot reach the server and has no saved library on this device yet.")
        } actions: {
            Button("Retry") { Task { await store.refresh() } }
                .buttonStyle(.borderedProminent)
            Button("Open Settings") { showSettings = true }
                .buttonStyle(.bordered)
        }
    }

    private func createNotebook() {
        let title = newNotebookTitle.trimmingCharacters(in: .whitespaces)
        newNotebookTitle = ""
        guard !title.isEmpty else { return }
        Task {
            _ = try? await store.client.createNotebook(title: title)
            await store.refresh()
        }
    }
}

private struct DocCell: View {
    let doc: PapierDoc
    @EnvironmentObject private var store: LibraryStore

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            ZStack {
                Color.white
                CachedRemoteImage(url: store.client.coverURL(doc)) { phase in
                    switch phase {
                    case .success(let img): img.resizable().scaledToFill()
                    case .empty, .failure:
                        Image(systemName: doc.isNotebook ? "pencil.and.outline" : "book.closed")
                            .font(.system(size: 34))
                            .foregroundStyle(.tertiary)
                    }
                }
            }
            .aspectRatio(3.0 / 4.0, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(.black.opacity(0.1)))
            .shadow(color: .black.opacity(0.10), radius: 5, y: 2)

            Text(doc.meta.title)
                .font(.callout.weight(.medium))
                .lineLimit(2, reservesSpace: true)
                .foregroundStyle(.primary)

            Text(doc.isNotebook ? "Notebook" : "\(doc.meta.pages ?? 0) pages")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }
}
