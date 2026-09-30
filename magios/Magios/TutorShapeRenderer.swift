import SwiftUI

enum TutorShapeRenderer {
    /// Matches `label(_:color:)`; wrapping must measure the font it draws.
    static let fontSizeForBoxLabel: Double = 15

    /// Registry the render path consults for a recipe. Injectable for tests;
    /// defaults to the shared (bundled + backend-refreshed) set.
    static var registry: TutorPrimitiveRegistry = .shared

    static func render(
        _ shape: TutorShape,
        in context: inout GraphicsContext,
        fittedRect: CGRect,
        screenshotSize: CGSize,
        progress: Double,
        labelYOffset: CGFloat = 0
    ) {
        // Data-driven path: a loaded recipe renders the shape (no hardcoded
        // switch, no app rebuild for a new primitive). The native switch below is
        // kept only as a belt-and-suspenders fallback for a type with no recipe
        // (e.g. an empty bundle / offline first launch before the initial fetch).
        if let recipe = registry.recipe(for: shape.type) {
            RecipeInterpreter.render(
                recipe,
                shape: shape,
                in: &context,
                fittedRect: fittedRect,
                screenshotSize: screenshotSize,
                progress: progress,
                labelYOffset: labelYOffset
            )
            return
        }

        renderNative(shape, in: &context, fittedRect: fittedRect,
                     screenshotSize: screenshotSize, progress: progress, labelYOffset: labelYOffset)
    }

    /// Legacy hand-coded per-type geometry — retained as a fallback only.
    private static func renderNative(
        _ shape: TutorShape,
        in context: inout GraphicsContext,
        fittedRect: CGRect,
        screenshotSize: CGSize,
        progress: Double,
        labelYOffset: CGFloat = 0
    ) {
        let space = shape.sourceSpace ?? .init(width: screenshotSize.width, height: screenshotSize.height)
        let strokeColor = color(shape.color ?? "#ffcc00").opacity(shape.opacity ?? 1)
        let fillColor = color(shape.fill ?? shape.color ?? "#ffcc00").opacity(shape.opacity ?? 0.22)
        let width = max(1, shape.strokeWidth ?? 4)
        let clampedProgress = min(max(progress, 0), 1)

        // `handwriting` is a text shape in the contract, but generators also send
        // it as stroke points. Route on what the shape actually carries: sent as
        // text it fell into the path branch, found no points, and drew nothing.
        let kind = shape.type.lowercased()
        let drawAs = (kind == "handwriting" && points(for: shape).isEmpty) ? "cursive_text" : kind

        switch drawAs {
        case "rect", "rectangle", "highlight",
             "free_body_body", "code_highlight", "stack_frame", "heap_object", "state_box",
             "flow_node", "memory_cell":
            guard let x = shape.x, let y = shape.y,
                  let w = shape.rectWidth, let h = shape.rectHeight else { return }
            let start = project(x, y, space, fittedRect)
            let end = project(x + w, y + h, space, fittedRect)
            let rect = CGRect(x: start.x, y: start.y, width: end.x - start.x, height: end.y - start.y)
            let path = Path(roundedRect: rect, cornerRadius: shape.type.lowercased() == "highlight" ? 8 : 6)
            if shape.type.lowercased() == "highlight" || shape.fill != nil {
                context.fill(path, with: .color(fillColor))
            }
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), lineWidth: CGFloat(width))
            // CS/box primitives (stack_frame, heap_object, …) carry a centered
            // label, WRAPPED to the box. `GraphicsContext.draw` never wraps, so
            // a long label used to run straight out of the box it names.
            if let text = shape.displayText, !text.isEmpty, clampedProgress > 0.5 {
                let usable = max(Double(fontSizeForBoxLabel) * 2, Double(rect.width) - 40)
                let lines = wrapLabel(text, toWidth: usable, fontSize: Double(fontSizeForBoxLabel))
                let lineHeight = CGFloat(fontSizeForBoxLabel) * 1.4
                for (index, line) in lines.enumerated() {
                    let dy = (CGFloat(index) - CGFloat(lines.count - 1) / 2) * lineHeight
                    context.draw(
                        label(line, color: strokeColor),
                        at: CGPoint(x: rect.midX, y: rect.midY + dy)
                    )
                }
            }
        case "line", "axis", "arrow", "side_label",
             "field_line", "trajectory", "measurement_tick",
             "vector_arrow", "force_arrow", "component_vector", "pointer_arrow", "flow_edge":
            guard let x1 = shape.startX, let y1 = shape.startY,
                  let x2 = shape.endX, let y2 = shape.endY else { return }
            let start = project(x1, y1, space, fittedRect)
            let end = project(x2, y2, space, fittedRect)
            var path = Path()
            path.move(to: start); path.addLine(to: end)
            let dashed = ["field_line", "trajectory"].contains(shape.type.lowercased())
            let style = StrokeStyle(lineWidth: CGFloat(width),
                                    dash: dashed ? [CGFloat(width) * 2, CGFloat(width) * 1.5] : [])
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), style: style)
            let arrowTypes: Set<String> = ["arrow", "vector_arrow", "force_arrow",
                                           "component_vector", "pointer_arrow", "flow_edge"]
            if arrowTypes.contains(shape.type.lowercased()), clampedProgress > 0.85 {
                drawArrowHead(from: start, to: end, color: strokeColor, width: width, context: &context)
            }
            if let text = shape.displayText, clampedProgress > 0.75 {
                context.draw(label(text, color: strokeColor), at: CGPoint(x: (start.x + end.x) / 2, y: (start.y + end.y) / 2 - 14))
            }
        case "circle":
            guard let cx = shape.cx ?? shape.x, let cy = shape.cy ?? shape.y,
                  let radius = shape.r else { return }
            let center = project(cx, cy, space, fittedRect)
            let edge = project(cx + radius, cy, space, fittedRect)
            let screenRadius = abs(edge.x - center.x)
            let path = Path(ellipseIn: CGRect(x: center.x - screenRadius, y: center.y - screenRadius,
                                             width: screenRadius * 2, height: screenRadius * 2))
            if shape.fill != nil { context.fill(path, with: .color(fillColor)) }
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), lineWidth: CGFloat(width))
        case "label", "callout", "formula", "unit_label", "timeline_tick":
            guard let x = shape.x, let y = shape.y, let text = shape.displayText else { return }
            var point = project(x, y, space, fittedRect); point.y += labelYOffset
            if shape.type.lowercased() == "callout" {
                let background = Path(roundedRect: CGRect(x: point.x - 8, y: point.y - 18,
                                                          width: min(260, CGFloat(text.count * 9 + 16)), height: 36),
                                      cornerRadius: 9)
                context.fill(background, with: .color(.black.opacity(0.78)))
            }
            if clampedProgress > 0.15 { context.draw(label(text, color: strokeColor), at: point, anchor: .leading) }
        case "path", "polygon", "polyline", "handwriting", "area_fill", "freehand":
            let points = points(for: shape).map { project($0.x, $0.y, space, fittedRect) }
            guard let first = points.first else { return }
            var path = Path(); path.move(to: first)
            for point in points.dropFirst() { path.addLine(to: point) }
            if ["polygon", "area_fill"].contains(shape.type.lowercased()) { path.closeSubpath() }
            if shape.type.lowercased() == "area_fill" || shape.fill != nil {
                context.fill(path, with: .color(fillColor))
            }
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), lineWidth: CGFloat(width))

        case "cursive_text":
            // Handwriting-style text via a built-in iOS cursive face (no bundled font).
            guard let x = shape.x, let y = shape.y, let text = shape.displayText ?? shape.d else { return }
            var point = project(x, y, space, fittedRect); point.y += labelYOffset
            if clampedProgress > 0.15 {
                let spaceWidth = shape.sourceSpace?.width ?? screenshotSize.width
                let scale = spaceWidth > 0 ? fittedRect.width / CGFloat(spaceWidth) : 1
                let pt = CGFloat(min(180, max(34, shape.fontSize ?? 92))) * scale
                context.draw(Text(text).font(.custom("SnellRoundhand-Bold", size: max(16, pt)))
                    .foregroundColor(strokeColor), at: point, anchor: .leading)
            }

        case "right_angle_marker":
            // Small L at the vertex: (x+s,y) → (x+s,y+s) → (x,y+s).
            guard let x = shape.x ?? shape.cx, let y = shape.y ?? shape.cy else { return }
            let s = shape.size ?? 28
            var path = Path()
            path.move(to: project(x + s, y, space, fittedRect))
            path.addLine(to: project(x + s, y + s, space, fittedRect))
            path.addLine(to: project(x, y + s, space, fittedRect))
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), lineWidth: CGFloat(width))

        case "angle_marker", "perpendicular_marker", "parallel_marker", "arc":
            // Arc centered at (cx,cy), radius r, start_angle→end_angle (degrees).
            guard let cx = shape.cx ?? shape.x, let cy = shape.cy ?? shape.y else { return }
            let r = shape.r ?? shape.size ?? 36
            let a1 = (shape.startAngle ?? 0) * .pi / 180
            let a2 = (shape.endAngle ?? 90) * .pi / 180
            let steps = 24
            var path = Path()
            for i in 0...steps {
                let t = a1 + (a2 - a1) * Double(i) / Double(steps)
                let p = project(cx + cos(t) * r, cy + sin(t) * r, space, fittedRect)
                if i == 0 { path.move(to: p) } else { path.addLine(to: p) }
            }
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), lineWidth: CGFloat(width))

        case "square_on_segment":
            // Square built on segment (x1,y1)→(x2,y2), on the chosen side.
            guard let x1 = shape.startX, let y1 = shape.startY,
                  let x2 = shape.endX, let y2 = shape.endY else { return }
            let dx = x2 - x1, dy = y2 - y1
            let len = (dx * dx + dy * dy).squareRoot()
            guard len > 0 else { return }
            let sign: Double = (shape.side == "right" || shape.side == "-1" || shape.orientation == "right") ? -1 : 1
            let nx = (-dy / len) * sign, ny = (dx / len) * sign
            let corners = [(x1, y1), (x2, y2), (x2 + nx * len, y2 + ny * len), (x1 + nx * len, y1 + ny * len)]
                .map { project($0.0, $0.1, space, fittedRect) }
            var path = Path(); path.move(to: corners[0])
            for c in corners.dropFirst() { path.addLine(to: c) }
            path.closeSubpath()
            if shape.fill != nil { context.fill(path, with: .color(fillColor)) }
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), lineWidth: CGFloat(width))

        case "curve":
            // Quadratic/cubic Bézier from (x1,y1) to (x2,y2) via control point(s).
            guard let x1 = shape.startX, let y1 = shape.startY,
                  let x2 = shape.endX, let y2 = shape.endY,
                  let c1x = shape.c1x, let c1y = shape.c1y else { return }
            let start = project(x1, y1, space, fittedRect)
            let end = project(x2, y2, space, fittedRect)
            let ctrl1 = project(c1x, c1y, space, fittedRect)
            var path = Path(); path.move(to: start)
            if let c2x = shape.c2x, let c2y = shape.c2y {
                path.addCurve(to: end, control1: ctrl1, control2: project(c2x, c2y, space, fittedRect))
            } else {
                path.addQuadCurve(to: end, control: ctrl1)
            }
            context.stroke(path.trimmedPath(from: 0, to: clampedProgress), with: .color(strokeColor), lineWidth: CGFloat(width))

        default:
            break
        }
    }

    private static func project(
        _ x: Double,
        _ y: Double,
        _ space: TutorShape.CoordinateSpace,
        _ fittedRect: CGRect
    ) -> CGPoint {
        tutorProject(point: CGPoint(x: x, y: y), coordinateSpace: space, fittedRect: fittedRect)
    }

    private static func points(for shape: TutorShape) -> [TutorShape.Point] {
        if let points = shape.points { return points }
        guard let d = shape.d else { return [] }
        let tokens = d.components(separatedBy: CharacterSet(charactersIn: "0123456789.-").inverted)
            .compactMap(Double.init)
        guard tokens.count >= 2 else { return [] }
        return stride(from: 0, to: tokens.count - 1, by: 2).map {
            TutorShape.Point(x: tokens[$0], y: tokens[$0 + 1])
        }
    }

    private static func drawArrowHead(
        from start: CGPoint,
        to end: CGPoint,
        color: Color,
        width: Double,
        context: inout GraphicsContext
    ) {
        let angle = atan2(end.y - start.y, end.x - start.x)
        let length: CGFloat = max(12, CGFloat(width * 4))
        var head = Path(); head.move(to: end)
        head.addLine(to: CGPoint(x: end.x - length * cos(angle - .pi / 6),
                                 y: end.y - length * sin(angle - .pi / 6)))
        head.move(to: end)
        head.addLine(to: CGPoint(x: end.x - length * cos(angle + .pi / 6),
                                 y: end.y - length * sin(angle + .pi / 6)))
        context.stroke(head, with: .color(color), lineWidth: CGFloat(width))
    }

    /// Paint layer for shapes that would otherwise occlude by arrival order.
    ///
    /// Mirrors `paintLayer` in `draw-overlay/+page.svelte`. An opaque
    /// background emitted after a label used to paint straight over it, because
    /// emission order was the only order and the model has no reason to reason
    /// about occlusion. Fills paint first, then strokes, then anything carrying
    /// text. Applied as a STABLE sort, so reveal sequence is preserved within a
    /// layer — this decides only what was previously arbitrary.
    static func paintLayer(_ shape: TutorShape) -> Int {
        // `displayText` is already `text ?? formula ?? label`.
        let text = shape.displayText ?? ""
        if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return 2 }
        let type = shape.type.lowercased()
        if shape.fill != nil
            || ["rect", "rectangle", "highlight", "area_fill"].contains(type) { return 0 }
        return 1
    }

    /// Greedy wrap against MEASURED width, mirroring `semanticBoxLabelLayout`.
    ///
    /// A box label was drawn as one centred `Text`, and `GraphicsContext.draw`
    /// does not wrap — so any label wider than its box simply extended past it.
    static func wrapLabel(_ text: String, toWidth width: Double, fontSize: Double) -> [String] {
        let words = text.split(whereSeparator: { $0.isWhitespace }).map(String.init)
        guard !words.isEmpty else { return [] }
        var lines: [String] = []
        var current = ""
        for word in words {
            if current.isEmpty { current = word; continue }
            let candidate = "\(current) \(word)"
            if measureLabelWidth(candidate, fontSize: fontSize) <= width {
                current = candidate
            } else {
                lines.append(current)
                current = word
            }
        }
        if !current.isEmpty { lines.append(current) }
        return lines
    }

    private static func measureLabelWidth(_ text: String, fontSize: Double) -> Double {
        RecipeInterpreter.measureLabel(text, fontSize: fontSize).width
    }

    private static func label(_ text: String, color: Color) -> Text {
        Text(text).font(.system(size: 15, weight: .semibold)).foregroundColor(color)
    }

    private static func color(_ raw: String) -> Color {
        switch raw.lowercased() {
        case "red": return .red
        case "orange": return .orange
        case "yellow": return .yellow
        case "green": return .green
        case "blue": return .blue
        case "purple": return .purple
        case "white": return .white
        case "black": return .black
        default: return Color(hex: raw)
        }
    }
}
