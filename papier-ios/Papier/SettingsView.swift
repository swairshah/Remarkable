// SettingsView.swift — where the papier cloud lives. The VM is reached
// over the tailnet: http://<remarkable-vm tailscale IP or MagicDNS>:8000.

import SwiftUI

struct SettingsView: View {
    @EnvironmentObject private var store: LibraryStore
    @Environment(\.dismiss) private var dismiss

    @State private var url: String = ""
    @State private var testResult: String?
    @State private var testing = false

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("http://100.x.y.z:8000", text: $url)
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.system(.body, design: .monospaced))
                } header: {
                    Text("Papier server")
                } footer: {
                    Text("The reMarkable cloud VM on your tailnet — the same one behind remarkable.exe.xyz. Requires Tailscale to be connected on this iPad.")
                }

                Section {
                    Button {
                        test()
                    } label: {
                        if testing { ProgressView() } else { Text("Test connection") }
                    }
                    .disabled(testing || !url.contains("://"))
                    if let testResult {
                        Text(testResult).font(.footnote)
                    }
                } footer: {
                    Text("Papier \(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?") (\(Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String ?? "?"))")
                }
            }
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        store.serverRoot = normalized(url)
                        dismiss()
                        Task { await store.refresh() }
                    }
                }
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .onAppear { url = store.serverRoot }
        }
    }

    private func normalized(_ s: String) -> String {
        var out = s.trimmingCharacters(in: .whitespacesAndNewlines)
        while out.hasSuffix("/") { out.removeLast() }
        return out
    }

    private func test() {
        testing = true
        testResult = nil
        let client = PapierClient(serverRoot: normalized(url))
        Task {
            defer { testing = false }
            do {
                let (lib, _) = try await client.library(etag: nil)
                testResult = "✓ Connected — \(lib?.docs.count ?? 0) documents (generation \(lib?.generation ?? "?"))"
            } catch {
                testResult = "✗ \(error.localizedDescription)"
            }
        }
    }
}
