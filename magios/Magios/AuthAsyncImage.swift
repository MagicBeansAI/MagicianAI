import SwiftUI
import UIKit

/// AsyncImage can't attach the Cloudflare Access headers, so an Access-gated
/// artifact URL would load the login wall instead of the image. This view
/// downloads the bytes with MagicianAccess.authorize() applied, then displays
/// them — the same authed-download trick DeepWorkPanel uses for artifacts.
struct AuthAsyncImage: View {
    let urlString: String
    var maxHeight: CGFloat = 260

    @State private var image: UIImage?
    @State private var failed = false
    @StateObject private var themeManager = ThemeManager.shared

    var body: some View {
        Group {
            if let image = image {
                Image(uiImage: image)
                    .resizable()
                    .scaledToFit()
                    .frame(maxWidth: .infinity)
                    .frame(maxHeight: maxHeight)
                    .cornerRadius(12)
            } else if failed {
                HStack(spacing: 6) {
                    Image(systemName: "photo").font(.caption)
                    Text("Image unavailable").font(.caption)
                }
                .foregroundColor(themeManager.secondaryTextColor)
                .frame(maxWidth: .infinity, minHeight: 80)
                .background(themeManager.surfaceColor)
                .cornerRadius(12)
            } else {
                ProgressView()
                    .frame(maxWidth: .infinity, minHeight: 120)
                    .background(themeManager.surfaceColor)
                    .cornerRadius(12)
            }
        }
        .onAppear(perform: load)
    }

    private func load() {
        guard image == nil, !failed else { return }
        // Absolute URLs pass through; a relative artifact path resolves against
        // the tunnel host so the Access headers still apply.
        let full = urlString.hasPrefix("http") ? urlString : "\(MagicianAccess.baseURL.absoluteString)\(urlString.hasPrefix("/") ? "" : "/")\(urlString)"
        guard let url = URL(string: full) else { failed = true; return }
        var request = URLRequest(url: url)
        MagicianAccess.authorize(&request)
        URLSession.shared.dataTask(with: request) { data, response, _ in
            let ok = (response as? HTTPURLResponse).map { (200...299).contains($0.statusCode) } ?? true
            if let data = data, ok, let img = UIImage(data: data) {
                DispatchQueue.main.async { self.image = img }
            } else {
                DispatchQueue.main.async { self.failed = true }
            }
        }.resume()
    }
}
