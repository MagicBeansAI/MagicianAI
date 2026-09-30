import SwiftUI
import UIKit

/// Generic interpreter that renders any `TutorRecipe` against the tutor canvas,
/// reproducing the hand-coded `TutorShapeRenderer` output exactly. Geometry is
/// resolved in the shape's coordinate space, projected via `tutorProject`, then
/// stroked/filled with draw-on `progress`. See
/// `docs/plans/2026-07-13-data-driven-tutor-primitives.md` Appendix A (normative).
enum RecipeInterpreter {
    // Hostile-input bounds (Appendix A).
    private static let maxOps = 64
    private static let maxPointsPerOp = 256

    // MARK: - Public render entry

    static func render(
        _ recipe: TutorRecipe,
        shape: TutorShape,
        in context: inout GraphicsContext,
        fittedRect: CGRect,
        screenshotSize: CGSize,
        progress: Double,
        labelYOffset: CGFloat = 0
    ) {
        let space = shape.sourceSpace ?? .init(width: screenshotSize.width, height: screenshotSize.height)
        let env = Environment(shape: shape, defaults: recipe.defaults)
        let clampedProgress = min(max(progress, 0), 1)

        let shapeStroke = mapColor(shape.color ?? "#ffcc00").opacity(shape.opacity ?? 1)
        let shapeWidth = max(1, shape.strokeWidth ?? 4)

        for op in recipe.draw.prefix(maxOps) {
            renderOp(
                op,
                env: env,
                shape: shape,
                space: space,
                in: &context,
                fittedRect: fittedRect,
                progress: clampedProgress,
                labelYOffset: labelYOffset,
                shapeStroke: shapeStroke,
                shapeWidth: shapeWidth
            )
        }
    }

    // MARK: - Testable geometry seam

    /// Space-coordinate geometry (pre-projection) each op produces for a shape —
    /// the seam tests assert against, since a `GraphicsContext` can't be
    /// inspected. Points are in the shape's coordinate space; label/text ops are
    /// omitted (they carry no stroked geometry). Ops skipped for a missing coord
    /// produce no entry.
    struct OpGeometry: Equatable {
        let op: String
        let points: [CGPoint]   // space coords
        let closed: Bool
    }

    static func geometry(for recipe: TutorRecipe, shape: TutorShape) -> [OpGeometry] {
        let env = Environment(shape: shape, defaults: recipe.defaults)
        var out: [OpGeometry] = []
        for op in recipe.draw.prefix(maxOps) {
            switch op.op.lowercased() {
            case "line":
                if let from = env.point(op.params["from"]), let to = env.point(op.params["to"]) {
                    out.append(OpGeometry(op: "line", points: [from, to], closed: false))
                }
            case "polyline", "polygon":
                let pts = env.points(op.params["points"], shape: shape)
                if !pts.isEmpty {
                    out.append(OpGeometry(op: op.op.lowercased(), points: pts,
                                          closed: op.op.lowercased() == "polygon"))
                }
            case "rect":
                if let x = env.number(op.params["x"]), let y = env.number(op.params["y"]),
                   let w = env.number(op.params["w"]), let h = env.number(op.params["h"]) {
                    out.append(OpGeometry(op: "rect",
                                          points: [CGPoint(x: x, y: y), CGPoint(x: x + w, y: y + h)],
                                          closed: true))
                }
            case "circle":
                if let cx = env.number(op.params["cx"]), let cy = env.number(op.params["cy"]),
                   let r = env.number(op.params["r"]) {
                    out.append(OpGeometry(op: "circle",
                                          points: [CGPoint(x: cx, y: cy), CGPoint(x: cx + r, y: cy)],
                                          closed: true))
                }
            case "arc":
                let pts = arcPoints(op, env: env)
                if !pts.isEmpty { out.append(OpGeometry(op: "arc", points: pts, closed: false)) }
            case "bezier":
                if let from = env.point(op.params["from"]), let to = env.point(op.params["to"]),
                   let c1 = env.point(op.params["c1"]) {
                    var pts = [from, c1]
                    if let c2 = env.point(op.params["c2"]) { pts.append(c2) }
                    pts.append(to)
                    out.append(OpGeometry(op: "bezier", points: pts, closed: false))
                }
            case "arrowhead":
                if let from = env.point(op.params["from"]), let to = env.point(op.params["to"]) {
                    out.append(OpGeometry(op: "arrowhead", points: [from, to], closed: false))
                }
            default:
                break   // label / cursive_label carry no stroked geometry
            }
        }
        return out
    }

    // MARK: - Per-op rendering

    private static func renderOp(
        _ op: RecipeOp,
        env: Environment,
        shape: TutorShape,
        space: TutorShape.CoordinateSpace,
        in context: inout GraphicsContext,
        fittedRect: CGRect,
        progress: Double,
        labelYOffset: CGFloat,
        shapeStroke: Color,
        shapeWidth: Double
    ) {
        func project(_ p: CGPoint) -> CGPoint {
            tutorProject(point: p, coordinateSpace: space, fittedRect: fittedRect)
        }

        // Resolved styling for this op.
        let width = max(1, env.number(op.params["width"]) ?? shapeWidth)
        let opColorName = env.stringValue(op.params["color"])
        let strokeColor: Color = {
            guard let name = opColorName else { return shapeStroke }
            let base = mapColor(name)
            return base.opacity(env.number(op.params["opacity"]) ?? shape.opacity ?? 1)
        }()
        let fillColor: Color = {
            let base = opColorName.map { mapColor($0) } ?? mapColor(shape.fill ?? shape.color ?? "#ffcc00")
            return base.opacity(env.number(op.params["opacity"]) ?? shape.opacity ?? 0.22)
        }()
        let dashed = env.bool(op.params["dashed"]) ?? false
        let doStroke = env.bool(op.params["stroke"]) ?? true

        switch op.op.lowercased() {
        case "line":
            guard let from = env.point(op.params["from"]), let to = env.point(op.params["to"]) else { return }
            var path = Path()
            path.move(to: project(from)); path.addLine(to: project(to))
            strokePath(path, dashed: dashed, width: width, color: strokeColor,
                       progress: progress, context: &context)

        case "polyline", "polygon":
            let pts = env.points(op.params["points"], shape: shape).prefix(maxPointsPerOp).map { project($0) }
            guard let first = pts.first else { return }
            var path = Path(); path.move(to: first)
            for p in pts.dropFirst() { path.addLine(to: p) }
            let isPolygon = op.op.lowercased() == "polygon"
            if isPolygon { path.closeSubpath() }
            if shouldFill(op, env: env, isFillable: isPolygon, shape: shape) {
                context.fill(path, with: .color(fillColor))
            }
            if doStroke {
                strokePath(path, dashed: dashed, width: width, color: strokeColor,
                           progress: progress, context: &context)
            }

        case "rect":
            guard let x = env.number(op.params["x"]), let y = env.number(op.params["y"]),
                  let w = env.number(op.params["w"]), let h = env.number(op.params["h"]) else { return }
            let start = project(CGPoint(x: x, y: y))
            let end = project(CGPoint(x: x + w, y: y + h))
            let rect = CGRect(x: start.x, y: start.y, width: end.x - start.x, height: end.y - start.y)
            let radius = env.number(op.params["radius"]) ?? 6
            let path = Path(roundedRect: rect, cornerRadius: CGFloat(radius))
            if shouldFill(op, env: env, isFillable: true, shape: shape) {
                context.fill(path, with: .color(fillColor))
            }
            if doStroke {
                strokePath(path, dashed: dashed, width: width, color: strokeColor,
                           progress: progress, context: &context)
            }

        case "circle":
            guard let cx = env.number(op.params["cx"]), let cy = env.number(op.params["cy"]),
                  let r = env.number(op.params["r"]) else { return }
            let center = project(CGPoint(x: cx, y: cy))
            let edge = project(CGPoint(x: cx + r, y: cy))
            let screenRadius = abs(edge.x - center.x)
            let path = Path(ellipseIn: CGRect(x: center.x - screenRadius, y: center.y - screenRadius,
                                              width: screenRadius * 2, height: screenRadius * 2))
            if shouldFill(op, env: env, isFillable: true, shape: shape) {
                context.fill(path, with: .color(fillColor))
            }
            if doStroke {
                strokePath(path, dashed: dashed, width: width, color: strokeColor,
                           progress: progress, context: &context)
            }

        case "arc":
            let pts = arcPoints(op, env: env).prefix(maxPointsPerOp).map { project($0) }
            guard let first = pts.first else { return }
            var path = Path(); path.move(to: first)
            for p in pts.dropFirst() { path.addLine(to: p) }
            strokePath(path, dashed: dashed, width: width, color: strokeColor,
                       progress: progress, context: &context)

        case "bezier":
            guard let from = env.point(op.params["from"]), let to = env.point(op.params["to"]),
                  let c1 = env.point(op.params["c1"]) else { return }
            var path = Path(); path.move(to: project(from))
            if let c2 = env.point(op.params["c2"]) {
                path.addCurve(to: project(to), control1: project(c1), control2: project(c2))
            } else {
                path.addQuadCurve(to: project(to), control: project(c1))
            }
            strokePath(path, dashed: dashed, width: width, color: strokeColor,
                       progress: progress, context: &context)

        case "arrowhead":
            guard progress > 0.85,
                  let from = env.point(op.params["from"]), let to = env.point(op.params["to"]) else { return }
            drawArrowHead(from: project(from), to: project(to),
                          color: strokeColor, width: width, context: &context)

        case "label":
            guard let text = env.text(op.params["text"], shape: shape),
                  let at = env.point(op.params["at"]) else { return }
            let threshold = env.number(op.params["threshold"]) ?? 0.15
            guard progress > threshold else { return }
            var point = project(at); point.y += labelYOffset
            let anchor = textAnchor(env.stringValue(op.params["anchor"]))
            context.draw(label(text, color: strokeColor), at: point, anchor: anchor)

        case "cursive_label":
            guard let text = env.text(op.params["text"], shape: shape),
                  let at = env.point(op.params["at"]) else { return }
            let threshold = env.number(op.params["threshold"]) ?? 0.15
            guard progress > threshold else { return }
            var point = project(at); point.y += labelYOffset
            let spaceWidth = space.width
            let scale = spaceWidth > 0 ? fittedRect.width / CGFloat(spaceWidth) : 1
            let pt = CGFloat(min(180, max(34, env.number(op.params["size"]) ?? 92))) * scale
            let anchor = textAnchor(env.stringValue(op.params["anchor"]))
            context.draw(Text(text).font(.custom("SnellRoundhand-Bold", size: max(16, pt)))
                .foregroundColor(strokeColor), at: point, anchor: anchor)

        default:
            break   // unknown op -> no-op (rest of the recipe still draws)
        }
    }

    // MARK: - Fill decision

    /// `rect`/`circle`/`polygon` fill iff `op.fill == true` OR
    /// (`op.fill != false` AND the shape carries a `fill`). Non-fillable ops never fill.
    private static func shouldFill(_ op: RecipeOp, env: Environment, isFillable: Bool, shape: TutorShape) -> Bool {
        guard isFillable else { return false }
        if let explicit = env.bool(op.params["fill"]) { return explicit }
        return shape.fill != nil
    }

    // MARK: - Arc sampling (24 segments in space coords)

    /// Arc points, circular or elliptical.
    ///
    /// `rx`/`ry` are optional and each falls back to `r`, so every existing arc
    /// recipe keeps its exact geometry. They exist because a solid drawn in
    /// projection needs an ellipse — the base of a cone or cylinder is a circle
    /// seen at an angle, and drawing it as a circle is the difference between a
    /// cone and a party hat.
    private static func arcPoints(_ op: RecipeOp, env: Environment) -> [CGPoint] {
        guard let cx = env.number(op.params["cx"]), let cy = env.number(op.params["cy"]) else {
            return []
        }
        let r = env.number(op.params["r"])
        guard let rx = env.number(op.params["rx"]) ?? r,
              let ry = env.number(op.params["ry"]) ?? r else { return [] }
        let a1 = (env.number(op.params["from"]) ?? 0) * .pi / 180
        let a2 = (env.number(op.params["to"]) ?? 90) * .pi / 180
        // A full ellipse needs more than the 24 steps a 90° arc was tuned for,
        // or the seam shows as a visible polygon edge.
        let sweep = abs(a2 - a1)
        let steps = max(24, min(96, Int((sweep / (.pi / 2) * 24).rounded(.up))))
        var pts: [CGPoint] = []
        for i in 0...steps {
            let t = a1 + (a2 - a1) * Double(i) / Double(steps)
            pts.append(CGPoint(x: cx + cos(t) * rx, y: cy + sin(t) * ry))
        }
        return pts
    }

    // MARK: - Stroke helper (draw-on progress + dash)

    private static func strokePath(
        _ path: Path, dashed: Bool, width: Double, color: Color,
        progress: Double, context: inout GraphicsContext
    ) {
        let style = StrokeStyle(lineWidth: CGFloat(width),
                                dash: dashed ? [CGFloat(width) * 2, CGFloat(width) * 1.5] : [])
        context.stroke(path.trimmedPath(from: 0, to: progress), with: .color(color), style: style)
    }

    // MARK: - Shared drawing helpers (mirror TutorShapeRenderer)

    private static func drawArrowHead(
        from start: CGPoint, to end: CGPoint, color: Color, width: Double,
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

    /// The size non-cursive labels render at. Shared with `measureLabel` so the
    /// box a recipe draws and the text it wraps can never be sized for
    /// different fonts.
    static let labelFontSize: Double = 15

    private static func label(_ text: String, color: Color) -> Text {
        Text(text)
            .font(.system(size: CGFloat(labelFontSize), weight: .semibold))
            .foregroundColor(color)
    }

    /// Measures label text with the font the renderer actually draws, so a
    /// recipe can size a background around glyphs instead of a character
    /// count — which cannot see glyph width, and clipped any long label.
    static func measureLabel(_ text: String, fontSize: Double) -> (width: Double, height: Double) {
        let font = UIFont.systemFont(ofSize: CGFloat(fontSize), weight: .semibold)
        let size = (text as NSString).size(withAttributes: [.font: font])
        return (Double(size.width), Double(size.height))
    }

    private static func textAnchor(_ raw: String?) -> UnitPoint {
        switch (raw ?? "center").lowercased() {
        case "leading": return .leading
        case "trailing": return .trailing
        default: return .center
        }
    }

    static func mapColor(_ raw: String) -> Color {
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

    // MARK: - Environment (field resolution + derived fields)

    /// Resolves identifiers per Appendix A: raw shape field -> injected derived
    /// field -> recipe `defaults`. Evaluates op params (numbers via
    /// `RecipeExpression`, coalesce chains, literals, and point pairs).
    struct Environment {
        let base: [String: Double]
        let derived: [String: Double]
        let defaults: [String: Double]

        init(shape: TutorShape, defaults: [String: Double]) {
            self.base = shape.numericEnvironment()
            self.defaults = defaults

            // Injected derived fields (so recipes avoid alias chains).
            var derived: [String: Double] = [:]
            let sx = shape.fromX ?? shape.x1
            let sy = shape.fromY ?? shape.y1
            let ex = shape.toX ?? shape.x2
            let ey = shape.toY ?? shape.y2
            if let sx { derived["sx"] = sx }
            if let sy { derived["sy"] = sy }
            if let ex { derived["ex"] = ex }
            if let ey { derived["ey"] = ey }
            // cx/cy prefer a raw cx/cy, else x/y.
            if let cx = shape.cx ?? shape.x { derived["cx"] = cx }
            if let cy = shape.cy ?? shape.y { derived["cy"] = cy }
            if let w = shape.w ?? shape.width { derived["w"] = w }
            if let h = shape.h ?? shape.height { derived["h"] = h }
            if let sx, let ex { derived["mx"] = (sx + ex) / 2 }
            if let sy, let ey { derived["my"] = (sy + ey) / 2 }
            let sideRight = (shape.side == "right" || shape.side == "-1" || shape.orientation == "right")
            derived["side_sign"] = sideRight ? -1 : 1
            if let text = shape.displayText ?? shape.d {
                derived["text_len"] = Double(text.count)
                // Measured glyph box, so a recipe can size a background around
                // real text instead of a character count. `text_len` stays for
                // recipes that want a count; sizing from it is what let a long
                // label run out of its own bubble.
                let fontSize = shape.fontSize ?? defaults["font_size"]
                    ?? RecipeInterpreter.labelFontSize
                let metrics = RecipeInterpreter.measureLabel(text, fontSize: fontSize)
                derived["text_w"] = metrics.width
                derived["text_h"] = metrics.height
                // `text_rise` is ink ABOVE the label's anchor point, which is
                // why it is half the box here and NOT the font ascent. This
                // renderer draws labels through `GraphicsContext.draw(at:
                // anchor:)`, and `.leading` is `UnitPoint(0, 0.5)` — vertically
                // CENTRED. The web renderer anchors on the baseline and sets
                // the same variable to the ascent. Anchor-relative is what lets
                // one shared recipe produce a correct box on both.
                derived["text_rise"] = metrics.height / 2
            }

            self.derived = derived
        }

        /// Resolve a bare identifier: raw field -> derived -> defaults.
        func resolve(_ name: String) -> Double? {
            base[name] ?? derived[name] ?? defaults[name]
        }

        /// A numeric op-param value (number literal or an expression/field ref).
        func number(_ value: RecipeValue?) -> Double? {
            guard let value else { return nil }
            switch value {
            case .number(let d): return d
            case .string(let s):
                let v = RecipeExpression.evaluate(s, resolve: { self.resolve($0) })
                if let v, v.isNaN { return nil }
                return v
            case .array: return nil
            }
        }

        /// A `[x, y]` point param — each component resolved independently.
        func point(_ value: RecipeValue?) -> CGPoint? {
            guard case let .array(items)? = value, items.count >= 2 else { return nil }
            guard let x = number(items[0]), let y = number(items[1]) else { return nil }
            return CGPoint(x: x, y: y)
        }

        /// A `points` param: either a literal `[[x,y]…]` array, or a field ref
        /// (`"points|d"`) pulling the shape's `points` array / parsing its `d` string.
        func points(_ value: RecipeValue?, shape: TutorShape) -> [CGPoint] {
            switch value {
            case .array(let items):
                return items.compactMap { self.point($0) }
            case .string:
                return recipeShapePoints(shape)
            default:
                return []
            }
        }

        /// The raw string of a param (a field name / literal), for `anchor`/`color`
        /// — not evaluated as an expression.
        func stringValue(_ value: RecipeValue?) -> String? {
            if case let .string(s)? = value { return s }
            return nil
        }

        /// Resolve a text param — a literal or a field-ref (`"text"`, `"text|d"`).
        func text(_ value: RecipeValue?, shape: TutorShape) -> String? {
            guard case let .string(s)? = value else { return nil }
            for token in s.split(separator: "|").map({ $0.trimmingCharacters(in: .whitespaces) }) {
                switch token {
                case "text": if let t = shape.displayText { return t }
                case "label": if let label = shape.label { return label }
                case "formula": if let formula = shape.formula { return formula }
                case "d": if let d = shape.d { return d }
                default: return token   // literal
                }
            }
            return nil
        }

        func bool(_ value: RecipeValue?) -> Bool? {
            switch value {
            case .number(let d): return d != 0
            case .string(let s):
                switch s.lowercased() {
                case "true": return true
                case "false": return false
                default: return nil
                }
            default: return nil
            }
        }
    }
}

/// Points list from a shape (its `points` array, else parsed from `d`). Mirrors
/// the private `points(for:)` helper in the native renderer.
private func recipeShapePoints(_ shape: TutorShape) -> [CGPoint] {
    if let points = shape.points { return points.map { CGPoint(x: $0.x, y: $0.y) } }
    guard let d = shape.d else { return [] }
    let tokens = d.components(separatedBy: CharacterSet(charactersIn: "0123456789.-").inverted)
        .compactMap(Double.init)
    guard tokens.count >= 2 else { return [] }
    return stride(from: 0, to: tokens.count - 1, by: 2).map {
        CGPoint(x: tokens[$0], y: tokens[$0 + 1])
    }
}
