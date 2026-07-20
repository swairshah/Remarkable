// HomeView.swift — papier's xochitl-like home grid: covers and titles.
// Inbound-only documents are already usable cloud documents; they are not
// presented as perpetually "syncing" while the tablet happens to be asleep.

import SwiftUI

struct HomeView: View {
    @EnvironmentObject private var store: LibraryStore
    @State private var showSettings = false
    @State private var newNotebookTitle = ""
    @State private var askNotebookTitle = false

    private let columns = [GridItem(.adaptive(minimum: 168, maximum: 230), spacing: 22)]

    var body: some View {
        NavigationStack {
            Group {
                if !store.configured {
                    unconfigured
                } else if store.docs.isEmpty && store.loading {
                    ProgressView("Loading library…")
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
            .navigationDestination(for: PapierDoc.self) { doc in
                DocumentView(doc: doc).environmentObject(store)
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
            LazyVGrid(columns: columns, spacing: 26) {
                ForEach(store.docs) { doc in
                    NavigationLink(value: doc) { DocCell(doc: doc) }
                        .buttonStyle(.plain)
                }
            }
            .padding(22)
        }
        .background(Color(uiColor: .systemGroupedBackground))
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
                AsyncImage(url: store.client.coverURL(doc)) { phase in
                    switch phase {
                    case .success(let img): img.resizable().scaledToFill()
                    case .empty, .failure:
                        Image(systemName: doc.isNotebook ? "pencil.and.outline" : "book.closed")
                            .font(.system(size: 34))
                            .foregroundStyle(.tertiary)
                    @unknown default: EmptyView()
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
