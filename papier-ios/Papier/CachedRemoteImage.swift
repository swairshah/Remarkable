import SwiftUI
import UIKit

enum CachedRemoteImagePhase {
    case empty
    case success(Image)
    case failure
}

/// AsyncImage-shaped view backed by Papier's durable offline image cache.
struct CachedRemoteImage<Content: View>: View {
    let url: URL?
    let content: (CachedRemoteImagePhase) -> Content

    @State private var image: UIImage?
    @State private var failed = false

    init(url: URL?, @ViewBuilder content: @escaping (CachedRemoteImagePhase) -> Content) {
        self.url = url
        self.content = content
    }

    var body: some View {
        content(phase)
            .task(id: url) {
                image = nil
                failed = false
                guard let url else {
                    failed = true
                    return
                }
                var displayedCachedImage = false
                if let hit = await OfflineImageCache.shared.cachedData(for: url),
                   let loaded = UIImage(data: hit.data) {
                    guard !Task.isCancelled else { return }
                    image = loaded
                    displayedCachedImage = true
                    if hit.isCurrentVersion { return }
                }

                do {
                    let data = try await OfflineImageCache.shared.fetchAndStore(url)
                    guard !Task.isCancelled, let loaded = UIImage(data: data) else { return }
                    image = loaded
                } catch {
                    guard !Task.isCancelled else { return }
                    failed = !displayedCachedImage
                }
            }
    }

    private var phase: CachedRemoteImagePhase {
        if let image { return .success(Image(uiImage: image)) }
        return failed ? .failure : .empty
    }
}
