import CoreImage
import CoreMedia
import Foundation

/// Downscales a ReplayKit video `CMSampleBuffer` (the broadcast's screen frame)
/// to a JPEG keyframe capped on its long edge, for the screen-observation frame
/// ingest. Frames are throttled at the call site (one every few seconds), so
/// this only runs occasionally.
final class BroadcastFrameConverter {
    private let context = CIContext()
    private let maxLongEdge: CGFloat
    private let colorSpace = CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB()

    init(maxLongEdge: CGFloat = 1280) {
        self.maxLongEdge = maxLongEdge
    }

    func jpeg(from sampleBuffer: CMSampleBuffer) -> Data? {
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return nil }
        var image = CIImage(cvPixelBuffer: pixelBuffer)
        let longEdge = max(image.extent.width, image.extent.height)
        if longEdge > maxLongEdge, longEdge > 0 {
            let scale = maxLongEdge / longEdge
            image = image.transformed(by: CGAffineTransform(scaleX: scale, y: scale))
        }
        return context.jpegRepresentation(of: image, colorSpace: colorSpace, options: [:])
    }
}
