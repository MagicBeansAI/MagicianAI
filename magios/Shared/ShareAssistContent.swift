import Foundation
import UIKit
import UniformTypeIdentifiers

struct ShareAssistImage: Equatable {
    let pngData: Data
    let width: Int
    let height: Int
    let filename: String
}

enum ShareAssistContent: Equatable {
    case text(String, sourceURL: URL?)
    case image(ShareAssistImage)
    case webpage(URL)
    case unsupported

    var previewText: String? {
        switch self {
        case .text(let text, _): return text
        case .webpage(let url): return url.absoluteString
        case .image, .unsupported: return nil
        }
    }
}

enum ShareAssistContentLoader {
    static func load(from inputItems: [NSExtensionItem]) async -> ShareAssistContent {
        let providers = inputItems.flatMap { $0.attachments ?? [] }
        let attributed = inputItems
            .compactMap { $0.attributedContentText?.string }
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .first { !$0.isEmpty }
        if providers.isEmpty {
            return attributed.map { .text($0, sourceURL: nil) } ?? .unsupported
        }
        let url = await firstURL(in: providers)

        if let attributed {
            return .text(attributed, sourceURL: url)
        }

        if let text = await firstText(in: providers) {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                if let url, trimmed == url.absoluteString { return .webpage(url) }
                return .text(trimmed, sourceURL: url)
            }
        }

        if let image = await firstImage(in: providers) { return .image(image) }
        if let url { return .webpage(url) }
        return .unsupported
    }

    static func normalizeImage(
        _ data: Data,
        filename: String = "shared-image.png",
        maximumDimension: CGFloat = 3_000
    ) -> ShareAssistImage? {
        guard let source = UIImage(data: data) else { return nil }
        let sourceSize = source.size
        guard sourceSize.width > 0, sourceSize.height > 0 else { return nil }
        let scale = min(1, maximumDimension / max(sourceSize.width, sourceSize.height))
        let targetSize = CGSize(
            width: max(1, floor(sourceSize.width * scale)),
            height: max(1, floor(sourceSize.height * scale))
        )
        let format = UIGraphicsImageRendererFormat.default()
        format.scale = 1
        format.opaque = false
        let normalized = UIGraphicsImageRenderer(size: targetSize, format: format).image { _ in
            source.draw(in: CGRect(origin: .zero, size: targetSize))
        }
        guard let png = normalized.pngData() else { return nil }
        return ShareAssistImage(
            pngData: png,
            width: Int(targetSize.width),
            height: Int(targetSize.height),
            filename: filename
        )
    }

    private static func firstText(in providers: [NSItemProvider]) async -> String? {
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
            if let value = await loadItem(provider, type: UTType.plainText.identifier) {
                if let text = value as? String { return text }
                if let text = value as? NSAttributedString { return text.string }
                if let data = value as? Data { return String(data: data, encoding: .utf8) }
                if let url = value as? URL { return url.absoluteString }
            }
        }
        return nil
    }

    private static func firstURL(in providers: [NSItemProvider]) async -> URL? {
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
            if let value = await loadItem(provider, type: UTType.url.identifier) {
                if let url = value as? URL { return url }
                if let text = value as? String { return URL(string: text) }
                if let data = value as? Data,
                   let text = String(data: data, encoding: .utf8) { return URL(string: text) }
            }
        }
        return nil
    }

    private static func firstImage(in providers: [NSItemProvider]) async -> ShareAssistImage? {
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.image.identifier) {
            guard let data = await loadData(provider, type: UTType.image.identifier) else { continue }
            let suggested = provider.suggestedName ?? "shared-image"
            let name = URL(fileURLWithPath: suggested).pathExtension.isEmpty
                ? "\(suggested).png"
                : suggested
            if let image = normalizeImage(data, filename: name) { return image }
        }
        return nil
    }

    private static func loadItem(_ provider: NSItemProvider, type: String) async -> NSSecureCoding? {
        await withCheckedContinuation { continuation in
            provider.loadItem(forTypeIdentifier: type, options: nil) { value, _ in
                continuation.resume(returning: value)
            }
        }
    }

    private static func loadData(_ provider: NSItemProvider, type: String) async -> Data? {
        await withCheckedContinuation { continuation in
            provider.loadDataRepresentation(forTypeIdentifier: type) { data, _ in
                continuation.resume(returning: data)
            }
        }
    }
}
