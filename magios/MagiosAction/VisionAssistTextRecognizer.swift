import Foundation
import UIKit
@preconcurrency import Vision

enum VisionAssistTextRecognizer {
    enum RecognitionError: LocalizedError {
        case invalidImage
        case noText

        var errorDescription: String? {
            switch self {
            case .invalidImage: return "This image could not be read."
            case .noText: return "No readable text was found in this image."
            }
        }
    }

    static func recognize(_ image: ShareAssistImage) async throws -> String {
        guard let cgImage = UIImage(data: image.pngData)?.cgImage else {
            throw RecognitionError.invalidImage
        }
        return try await Task.detached(priority: .userInitiated) {
            try Task.checkCancellation()
            let request = VNRecognizeTextRequest()
            request.recognitionLevel = .accurate
            request.usesLanguageCorrection = true
            request.automaticallyDetectsLanguage = true
            try VNImageRequestHandler(cgImage: cgImage).perform([request])
            try Task.checkCancellation()
            let observations = (request.results ?? []).sorted {
                let verticalDelta = $0.boundingBox.midY - $1.boundingBox.midY
                if abs(verticalDelta) > 0.02 { return verticalDelta > 0 }
                return $0.boundingBox.minX < $1.boundingBox.minX
            }
            let text = observations
                .compactMap { $0.topCandidates(1).first?.string }
                .joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            guard !text.isEmpty else {
                throw RecognitionError.noText
            }
            return text
        }.value
    }
}
