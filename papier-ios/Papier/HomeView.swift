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
    @State private var libraryAlert: LibraryAlert?
    @State private var deletingID: String?
    @AppStorage("librarySort") private var sortRaw = LibrarySort.recent.rawValue

    private let columns = [GridItem(.adaptive(minimum: 168, maximum: 230), spacing: 22)]

    private var librarySort: LibrarySort { LibrarySort(rawValue: sortRaw) ?? .recent }
    private var displayedDocs: [PapierDoc] { librarySort.sorted(store.docs) }

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
                ToolbarItem(placement: .topBarLeading) {
                    Menu {
                        Picker("Sort documents", selection: $sortRaw) {
                            ForEach(LibrarySort.allCases, id: \.self) { option in
                                Label(option.label, systemImage: option.systemImage)
                                    .tag(option.rawValue)
                            }
                        }
                    } label: {
                        Label("Sort: \(librarySort.label)", systemImage: "arrow.up.arrow.down")
                    }
                    .accessibilityLabel("Sort documents, \(librarySort.label)")
                }
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
                    ForEach(displayedDocs) { doc in
                        documentTile(doc)
                    }
                }
            }
            .padding(22)
        }
        .background(Color(uiColor: .systemGroupedBackground))
        .alert(item: $libraryAlert) { alert in
            switch alert {
            case .delete(let doc):
                Alert(
                    title: Text("Delete “\(doc.meta.title)”?"),
                    message: Text("It disappears from Papier and the web now, then from the reMarkable after its next sync. Deleted documents remain recoverable from Papier trash."),
                    primaryButton: .destructive(Text("Delete")) { deleteDocument(doc) },
                    secondaryButton: .cancel()
                )
            case .error(let message):
                Alert(
                    title: Text("Couldn’t Delete Document"),
                    message: Text(message),
                    dismissButton: .default(Text("OK"))
                )
            }
        }
    }

    private func documentTile(_ doc: PapierDoc) -> some View {
        ZStack(alignment: .topTrailing) {
            Button { openedDoc = doc } label: { DocCell(doc: doc, librarySort: librarySort) }
                .buttonStyle(DocumentPressStyle())

            Menu {
                Button(role: .destructive) {
                    libraryAlert = .delete(doc)
                } label: {
                    Label("Delete", systemImage: "trash")
                }
            } label: {
                Image(systemName: deletingID == doc.id ? "hourglass" : "ellipsis")
                    .font(.system(size: 15, weight: .semibold))
                    .frame(width: 32, height: 32)
                    .background(.thinMaterial, in: Circle())
                    .frame(width: 44, height: 44)
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Actions for \(doc.meta.title)")
            .disabled(deletingID == doc.id)
            .padding(4)
        }
        .contextMenu {
            Button(role: .destructive) {
                libraryAlert = .delete(doc)
            } label: {
                Label("Delete", systemImage: "trash")
            }
        }
        .opacity(deletingID == doc.id ? 0.55 : 1)
        .allowsHitTesting(deletingID != doc.id)
        .animation(.easeOut(duration: 0.15), value: deletingID == doc.id)
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

    private func deleteDocument(_ doc: PapierDoc) {
        deletingID = doc.id
        Task {
            do {
                try await store.deleteDocument(doc)
            } catch {
                libraryAlert = .error(error.localizedDescription)
            }
            deletingID = nil
        }
    }
}

private enum LibraryAlert: Identifiable {
    case delete(PapierDoc)
    case error(String)

    var id: String {
        switch self {
        case .delete(let doc): "delete-\(doc.id)"
        case .error(let message): "error-\(message)"
        }
    }
}

private struct DocumentPressStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(
                reduceMotion ? nil : .easeOut(duration: 0.12),
                value: configuration.isPressed
            )
    }
}

private struct DocCell: View {
    let doc: PapierDoc
    let librarySort: LibrarySort
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

            Text(detail)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
    }

    private var detail: String {
        let kind = doc.isNotebook ? "Notebook" : "\(doc.meta.pages ?? 0) pages"
        let date = librarySort == .added ? (doc.addedDate ?? doc.modifiedAt) : doc.modifiedAt
        guard let date else { return kind }
        let verb = librarySort == .added ? "Added" : "Updated"
        return "\(kind) · \(verb) \(date.formatted(.relative(presentation: .named)))"
    }
}
