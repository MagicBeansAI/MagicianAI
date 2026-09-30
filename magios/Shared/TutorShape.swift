import Foundation
import CoreGraphics

/// Codable representation of the backend `screen-draw` `shape_json` contract.
/// Geometry is intentionally optional and unknown `type` values still decode so
/// newer backend shapes can be ignored safely by older clients.
public struct TutorShape: Codable, Equatable {
    public struct CoordinateSpace: Codable, Equatable {
        public let width: Double
        public let height: Double

        public init(width: Double, height: Double) {
            self.width = width
            self.height = height
        }
    }

    public struct Point: Codable, Equatable {
        public let x: Double
        public let y: Double

        public init(x: Double, y: Double) {
            self.x = x
            self.y = y
        }

        public init(from decoder: Decoder) throws {
            if var values = try? decoder.unkeyedContainer() {
                x = try values.decode(Double.self)
                y = try values.decode(Double.self)
                return
            }
            let values = try decoder.container(keyedBy: PointKeys.self)
            x = try values.decode(Double.self, forKey: .x)
            y = try values.decode(Double.self, forKey: .y)
        }

        public func encode(to encoder: Encoder) throws {
            var values = encoder.unkeyedContainer()
            try values.encode(x)
            try values.encode(y)
        }

        private enum PointKeys: String, CodingKey { case x, y }
    }

    public let type: String
    public let id: String?
    public let x: Double?
    public let y: Double?
    public let w: Double?
    public let h: Double?
    public let width: Double?
    public let height: Double?
    public let x1: Double?
    public let y1: Double?
    public let x2: Double?
    public let y2: Double?
    public let fromX: Double?
    public let fromY: Double?
    public let toX: Double?
    public let toY: Double?
    public let cx: Double?
    public let cy: Double?
    public let r: Double?
    /// Elliptical radii. Absent means circular; recipes coalesce `rx|r|size`.
    public let rx: Double?
    public let ry: Double?
    public let points: [Point]?
    public let d: String?
    public let text: String?
    public let label: String?
    public let formula: String?
    public let color: String?
    public let fill: String?
    public let strokeWidth: Double?
    public let opacity: Double?
    public let coordinateSpace: CoordinateSpace?
    public let coordinateSpaceName: String?
    public let captureImageSize: CoordinateSpace?
    public let storyboardStepId: String?
    public let tutorStepLabel: String?
    public let stepLabel: String?
    public let narration: String?
    public let delayMs: Double?
    public let durationMs: Double?
    public let revealOrder: Int?
    public let animate: Bool?
    public let waitForVoice: Bool?
    public let children: [TutorShape]?
    // Educational-primitive geometry (web parity).
    public let size: Double?          // right_angle_marker / angle_marker / marker radius
    public let startAngle: Double?    // angle_marker / arc (degrees)
    public let endAngle: Double?
    public let side: String?          // square_on_segment orientation ("left"/"right"/"-1")
    public let orientation: String?
    public let fontSize: Double?      // unit_label / cursive_text
    public let c1x: Double?           // curve control point 1
    public let c1y: Double?
    public let c2x: Double?           // curve control point 2 (cubic)
    public let c2y: Double?
    // Lifecycle (web parity): per-shape expiry + persistence controls.
    public let ttlMs: Double?         // per-shape time-to-live; absent = overlay-level expiry only
    public let persist: Bool?         // never auto-expire this shape
    public let persistUntilStep: String?  // keep until this storyboard step arrives
    public let clearPrevious: Bool?   // clear non-persistent shapes before drawing this one

    public var caption: String? { tutorStepLabel ?? stepLabel ?? narration }
    public var displayText: String? { text ?? formula ?? label }
    public var sourceSpace: CoordinateSpace? { coordinateSpace ?? captureImageSize }
    public var rectWidth: Double? { w ?? width }
    public var rectHeight: Double? { h ?? height }
    public var startX: Double? { fromX ?? x1 }
    public var startY: Double? { fromY ?? y1 }
    public var endX: Double? { toX ?? x2 }
    public var endY: Double? { toY ?? y2 }

    /// Every present raw numeric field keyed by its JSON name — the base
    /// environment the recipe interpreter resolves identifiers against (before
    /// injected derived fields and recipe defaults). Only defined values are
    /// included so a coalesce chain can fall through to the next operand.
    public func numericEnvironment() -> [String: Double] {
        var env: [String: Double] = [:]
        func put(_ key: String, _ value: Double?) { if let value { env[key] = value } }
        put("x", x); put("y", y)
        put("w", w); put("h", h)
        put("width", width); put("height", height)
        put("x1", x1); put("y1", y1); put("x2", x2); put("y2", y2)
        put("from_x", fromX); put("from_y", fromY)
        put("to_x", toX); put("to_y", toY)
        put("cx", cx); put("cy", cy); put("r", r)
        // An ellipse is a first-class request, not a circle that lost
        // precision. Unaddressable before this, an `arc` asking for 150x38
        // fell through to the angle_marker default `size: 36`.
        put("rx", rx); put("ry", ry)
        put("stroke_width", strokeWidth); put("opacity", opacity)
        put("size", size)
        put("start_angle", startAngle); put("end_angle", endAngle)
        put("font_size", fontSize)
        put("c1x", c1x); put("c1y", c1y); put("c2x", c2x); put("c2y", c2y)
        return env
    }

    private enum CodingKeys: String, CodingKey {
        case type, id, x, y, w, h, width, height, x1, y1, x2, y2, cx, cy, r, rx, ry
        case fromX = "from_x"
        case fromY = "from_y"
        case toX = "to_x"
        case toY = "to_y"
        case points, d, text, label, formula, color, fill
        case strokeWidth = "stroke_width"
        case opacity
        case coordinateSpace = "coordinate_space"
        case captureImageSize = "capture_image_size"
        case storyboardStepId = "storyboard_step_id"
        case tutorStepLabel = "tutor_step_label"
        case stepLabel = "step_label"
        case narration
        case delayMs = "delay_ms"
        case durationMs = "duration_ms"
        case revealOrder = "reveal_order"
        case waitForVoice = "wait_for_voice"
        case animate, children, shapes
        case size, side, orientation
        case startAngle = "start_angle"
        case endAngle = "end_angle"
        case fontSize = "font_size"
        case c1x, c1y, c2x, c2y
        case control1X = "control1_x"
        case control1Y = "control1_y"
        case control2X = "control2_x"
        case control2Y = "control2_y"
        case controlX = "control_x"
        case controlY = "control_y"
        case ttlMs = "ttl_ms"
        case persist
        case persistUntilStep = "persist_until_step"
        case clearPrevious = "clear_previous"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = try c.decode(String.self, forKey: .type)
        id = try c.decodeIfPresent(String.self, forKey: .id)
        x = try c.decodeIfPresent(Double.self, forKey: .x)
        y = try c.decodeIfPresent(Double.self, forKey: .y)
        w = try c.decodeIfPresent(Double.self, forKey: .w)
        h = try c.decodeIfPresent(Double.self, forKey: .h)
        width = try c.decodeIfPresent(Double.self, forKey: .width)
        height = try c.decodeIfPresent(Double.self, forKey: .height)
        x1 = try c.decodeIfPresent(Double.self, forKey: .x1)
        y1 = try c.decodeIfPresent(Double.self, forKey: .y1)
        x2 = try c.decodeIfPresent(Double.self, forKey: .x2)
        y2 = try c.decodeIfPresent(Double.self, forKey: .y2)
        fromX = try c.decodeIfPresent(Double.self, forKey: .fromX)
        fromY = try c.decodeIfPresent(Double.self, forKey: .fromY)
        toX = try c.decodeIfPresent(Double.self, forKey: .toX)
        toY = try c.decodeIfPresent(Double.self, forKey: .toY)
        cx = try c.decodeIfPresent(Double.self, forKey: .cx)
        cy = try c.decodeIfPresent(Double.self, forKey: .cy)
        r = try c.decodeIfPresent(Double.self, forKey: .r)
        rx = try c.decodeIfPresent(Double.self, forKey: .rx)
        ry = try c.decodeIfPresent(Double.self, forKey: .ry)
        points = try c.decodeIfPresent([Point].self, forKey: .points)
        d = try c.decodeIfPresent(String.self, forKey: .d)
        text = try c.decodeIfPresent(String.self, forKey: .text)
        label = try c.decodeIfPresent(String.self, forKey: .label)
        formula = try c.decodeIfPresent(String.self, forKey: .formula)
        color = try c.decodeIfPresent(String.self, forKey: .color)
        fill = try c.decodeIfPresent(String.self, forKey: .fill)
        strokeWidth = try c.decodeIfPresent(Double.self, forKey: .strokeWidth)
        opacity = try c.decodeIfPresent(Double.self, forKey: .opacity)
        coordinateSpace = try? c.decode(CoordinateSpace.self, forKey: .coordinateSpace)
        coordinateSpaceName = try? c.decode(String.self, forKey: .coordinateSpace)
        captureImageSize = try c.decodeIfPresent(CoordinateSpace.self, forKey: .captureImageSize)
        storyboardStepId = try c.decodeIfPresent(String.self, forKey: .storyboardStepId)
        tutorStepLabel = try c.decodeIfPresent(String.self, forKey: .tutorStepLabel)
        stepLabel = try c.decodeIfPresent(String.self, forKey: .stepLabel)
        narration = try c.decodeIfPresent(String.self, forKey: .narration)
        delayMs = try c.decodeIfPresent(Double.self, forKey: .delayMs)
        durationMs = try c.decodeIfPresent(Double.self, forKey: .durationMs)
        revealOrder = try c.decodeIfPresent(Int.self, forKey: .revealOrder)
        animate = try c.decodeIfPresent(Bool.self, forKey: .animate)
        waitForVoice = try c.decodeIfPresent(Bool.self, forKey: .waitForVoice)
        children = try c.decodeIfPresent([TutorShape].self, forKey: .children)
            ?? c.decodeIfPresent([TutorShape].self, forKey: .shapes)
        size = try c.decodeIfPresent(Double.self, forKey: .size)
        startAngle = try c.decodeIfPresent(Double.self, forKey: .startAngle)
        endAngle = try c.decodeIfPresent(Double.self, forKey: .endAngle)
        side = try c.decodeIfPresent(String.self, forKey: .side)
        orientation = try c.decodeIfPresent(String.self, forKey: .orientation)
        fontSize = try c.decodeIfPresent(Double.self, forKey: .fontSize)
        c1x = try (c.decodeIfPresent(Double.self, forKey: .c1x) ?? c.decodeIfPresent(Double.self, forKey: .control1X) ?? c.decodeIfPresent(Double.self, forKey: .controlX))
        c1y = try (c.decodeIfPresent(Double.self, forKey: .c1y) ?? c.decodeIfPresent(Double.self, forKey: .control1Y) ?? c.decodeIfPresent(Double.self, forKey: .controlY))
        c2x = try (c.decodeIfPresent(Double.self, forKey: .c2x) ?? c.decodeIfPresent(Double.self, forKey: .control2X))
        c2y = try (c.decodeIfPresent(Double.self, forKey: .c2y) ?? c.decodeIfPresent(Double.self, forKey: .control2Y))
        ttlMs = try c.decodeIfPresent(Double.self, forKey: .ttlMs)
        persist = try c.decodeIfPresent(Bool.self, forKey: .persist)
        persistUntilStep = try c.decodeIfPresent(String.self, forKey: .persistUntilStep)
        clearPrevious = try c.decodeIfPresent(Bool.self, forKey: .clearPrevious)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(type, forKey: .type)
        try c.encodeIfPresent(id, forKey: .id)
        try c.encodeIfPresent(x, forKey: .x); try c.encodeIfPresent(y, forKey: .y)
        try c.encodeIfPresent(w, forKey: .w); try c.encodeIfPresent(h, forKey: .h)
        try c.encodeIfPresent(width, forKey: .width); try c.encodeIfPresent(height, forKey: .height)
        try c.encodeIfPresent(x1, forKey: .x1); try c.encodeIfPresent(y1, forKey: .y1)
        try c.encodeIfPresent(x2, forKey: .x2); try c.encodeIfPresent(y2, forKey: .y2)
        try c.encodeIfPresent(fromX, forKey: .fromX); try c.encodeIfPresent(fromY, forKey: .fromY)
        try c.encodeIfPresent(toX, forKey: .toX); try c.encodeIfPresent(toY, forKey: .toY)
        try c.encodeIfPresent(cx, forKey: .cx); try c.encodeIfPresent(cy, forKey: .cy)
        try c.encodeIfPresent(r, forKey: .r)
        try c.encodeIfPresent(rx, forKey: .rx); try c.encodeIfPresent(ry, forKey: .ry)
        try c.encodeIfPresent(points, forKey: .points)
        try c.encodeIfPresent(d, forKey: .d); try c.encodeIfPresent(text, forKey: .text)
        try c.encodeIfPresent(label, forKey: .label); try c.encodeIfPresent(formula, forKey: .formula)
        try c.encodeIfPresent(color, forKey: .color); try c.encodeIfPresent(fill, forKey: .fill)
        try c.encodeIfPresent(strokeWidth, forKey: .strokeWidth); try c.encodeIfPresent(opacity, forKey: .opacity)
        if let coordinateSpace { try c.encode(coordinateSpace, forKey: .coordinateSpace) }
        else { try c.encodeIfPresent(coordinateSpaceName, forKey: .coordinateSpace) }
        try c.encodeIfPresent(captureImageSize, forKey: .captureImageSize)
        try c.encodeIfPresent(storyboardStepId, forKey: .storyboardStepId)
        try c.encodeIfPresent(tutorStepLabel, forKey: .tutorStepLabel)
        try c.encodeIfPresent(stepLabel, forKey: .stepLabel); try c.encodeIfPresent(narration, forKey: .narration)
        try c.encodeIfPresent(delayMs, forKey: .delayMs); try c.encodeIfPresent(durationMs, forKey: .durationMs)
        try c.encodeIfPresent(revealOrder, forKey: .revealOrder); try c.encodeIfPresent(animate, forKey: .animate)
        try c.encodeIfPresent(waitForVoice, forKey: .waitForVoice)
        try c.encodeIfPresent(children, forKey: .children)
        try c.encodeIfPresent(size, forKey: .size)
        try c.encodeIfPresent(startAngle, forKey: .startAngle); try c.encodeIfPresent(endAngle, forKey: .endAngle)
        try c.encodeIfPresent(side, forKey: .side); try c.encodeIfPresent(orientation, forKey: .orientation)
        try c.encodeIfPresent(fontSize, forKey: .fontSize)
        try c.encodeIfPresent(c1x, forKey: .c1x); try c.encodeIfPresent(c1y, forKey: .c1y)
        try c.encodeIfPresent(c2x, forKey: .c2x); try c.encodeIfPresent(c2y, forKey: .c2y)
        try c.encodeIfPresent(ttlMs, forKey: .ttlMs); try c.encodeIfPresent(persist, forKey: .persist)
        try c.encodeIfPresent(persistUntilStep, forKey: .persistUntilStep)
        try c.encodeIfPresent(clearPrevious, forKey: .clearPrevious)
    }
}
