import Foundation
import CoreGraphics

public func tutorFittedRect(imageSize: CGSize, in canvasSize: CGSize) -> CGRect {
    guard imageSize.width > 0, imageSize.height > 0, canvasSize.width > 0, canvasSize.height > 0 else {
        return .zero
    }
    let scale = min(canvasSize.width / imageSize.width, canvasSize.height / imageSize.height)
    let size = CGSize(width: imageSize.width * scale, height: imageSize.height * scale)
    return CGRect(
        x: (canvasSize.width - size.width) / 2,
        y: (canvasSize.height - size.height) / 2,
        width: size.width,
        height: size.height
    )
}

public func tutorProject(
    point: CGPoint,
    coordinateSpace: TutorShape.CoordinateSpace,
    fittedRect: CGRect
) -> CGPoint {
    guard coordinateSpace.width > 0, coordinateSpace.height > 0 else { return fittedRect.origin }
    let x = min(max(Double(point.x), 0), coordinateSpace.width)
    let y = min(max(Double(point.y), 0), coordinateSpace.height)
    return CGPoint(
        x: fittedRect.minX + CGFloat(x / coordinateSpace.width) * fittedRect.width,
        y: fittedRect.minY + CGFloat(y / coordinateSpace.height) * fittedRect.height
    )
}

/// Text-label overlap avoidance (web `applyTextLabelSpacing`): nudges nearby text
/// labels vertically so they don't stack on top of each other. Deterministic —
/// shapes are processed in array order; each label that would collide with an
/// already-placed one is pushed down a row until clear.
public enum TutorLabelLayout {
    public static let rowThreshold: CGFloat = 20
    public static let rowHeight: CGFloat = 24   // must exceed rowThreshold so one push clears a row
    public static let horizontalProximity: CGFloat = 140

    public static func isTextLike(_ type: String) -> Bool {
        ["label", "callout", "formula", "unit_label", "timeline_tick", "cursive_text"]
            .contains(type.lowercased())
    }

    public static func offsets(
        for shapes: [(id: UUID, shape: TutorShape)],
        fittedRect: CGRect,
        fallbackSpace: CGSize
    ) -> [UUID: CGFloat] {
        var placed: [CGPoint] = []
        var result: [UUID: CGFloat] = [:]
        for entry in shapes {
            guard isTextLike(entry.shape.type), let x = entry.shape.x, let y = entry.shape.y else { continue }
            let space = entry.shape.sourceSpace
                ?? .init(width: Double(fallbackSpace.width), height: Double(fallbackSpace.height))
            let base = tutorProject(point: CGPoint(x: x, y: y), coordinateSpace: space, fittedRect: fittedRect)
            var offset: CGFloat = 0
            while placed.contains(where: {
                abs((base.y + offset) - $0.y) < rowThreshold && abs(base.x - $0.x) < horizontalProximity
            }) {
                offset += rowHeight
            }
            placed.append(CGPoint(x: base.x, y: base.y + offset))
            result[entry.id] = offset
        }
        return result
    }
}

public struct TutorRevealItem: Identifiable, Equatable {
    public let id: UUID
    public let shape: TutorShape
    public let delayMs: Double
    public let durationMs: Double

    public init(shape: TutorShape, delayMs: Double, durationMs: Double) {
        id = UUID()
        self.shape = shape
        self.delayMs = delayMs
        self.durationMs = durationMs
    }
}

public struct TutorRevealState {
    public private(set) var items: [TutorRevealItem] = []
    public static let groupStaggerMs = 700.0

    public init() {}

    @discardableResult
    public mutating func ingest(_ shape: TutorShape) -> [TutorRevealItem] {
        if shape.type.lowercased() == "clear" {
            items.removeAll()
            return []
        }
        let newItems = Self.expand(shape)
        items.append(contentsOf: newItems)
        return newItems
    }

    public mutating func reset() { items.removeAll() }

    /// Registry the reveal gate consults. Injectable for tests; defaults to the
    /// shared (bundled + backend-refreshed) set — mirrors `TutorShapeRenderer.registry`.
    /// Internal (not public) since `TutorPrimitiveRegistry` is internal; tests reach
    /// it via `@testable import`.
    static var registry: TutorPrimitiveRegistry = .shared

    /// Whether a shape `type` renders. Derived from the loaded recipe set — never
    /// a hand-maintained allowlist — so a new primitive is a new recipe file, not
    /// new code.
    public static func isSupported(_ type: String) -> Bool {
        registry.isSupported(type)
    }

    private static func expand(_ shape: TutorShape) -> [TutorRevealItem] {
        if shape.type.lowercased() == "group" {
            let children = (shape.children ?? []).enumerated().sorted { lhs, rhs in
                (lhs.element.revealOrder ?? lhs.offset) < (rhs.element.revealOrder ?? rhs.offset)
            }
            return children.enumerated().flatMap { revealIndex, child in
                let nested = expand(child.element)
                let groupDelay = (shape.delayMs ?? 0) + Double(revealIndex) * groupStaggerMs
                return nested.map {
                    TutorRevealItem(
                        shape: $0.shape,
                        delayMs: groupDelay + $0.delayMs,
                        durationMs: $0.durationMs
                    )
                }
            }
        }
        guard isSupported(shape.type) else { return [] }
        return [TutorRevealItem(
            shape: shape,
            delayMs: max(0, shape.delayMs ?? 0),
            durationMs: shape.animate == false ? 0 : max(0, shape.durationMs ?? 450)
        )]
    }
}
