import Foundation

/// Pure, side-effect-free expression evaluator for tutor-primitive recipes.
///
/// Grammar (identical to the TS/Rust interpreters):
/// - number literals, identifiers (resolved via `resolve`), parentheses `( )`
/// - binary `+ - * /`, unary `-`
/// - coalesce `|` — **lowest precedence**, "first defined operand"
/// - functions: `sin cos tan sqrt abs min max deg rad`
///   (`deg` = degrees→radians, `rad` = radians→degrees)
///
/// A bare identifier resolves via `resolve`; an undefined operand *inside* a
/// coalesce chain is skipped, while an undefined identifier *outside* a chain
/// makes the whole expression `nil` (the caller then skips the op if the coord
/// is required). Recursion depth is guarded at <= 32. There is no code
/// execution — only arithmetic over a fixed function set.
public enum RecipeExpression {
    private static let maxDepth = 32

    /// Evaluate `expr` to a `Double`, resolving bare identifiers via `resolve`.
    /// Returns `nil` for a syntax error or an undefined-outside-coalesce result.
    public static func evaluate(_ expr: String, resolve: @escaping (String) -> Double?) -> Double? {
        var parser = Parser(expr, resolve: resolve)
        guard let value = parser.parseCoalesce(depth: 0) else { return nil }
        // Reject trailing garbage — a malformed expression evaluates to nil.
        parser.skipSpaces()
        guard parser.isAtEnd else { return nil }
        return value
    }

    // MARK: - Parser

    private struct Parser {
        let chars: [Character]
        var pos: Int = 0
        let resolve: (String) -> Double?

        init(_ s: String, resolve: @escaping (String) -> Double?) {
            self.chars = Array(s)
            self.resolve = resolve
        }

        var isAtEnd: Bool { pos >= chars.count }

        mutating func skipSpaces() {
            while pos < chars.count, chars[pos] == " " || chars[pos] == "\t" { pos += 1 }
        }

        func peek() -> Character? { pos < chars.count ? chars[pos] : nil }

        // coalesce `|` — lowest precedence, first-defined-operand.
        mutating func parseCoalesce(depth: Int) -> Double? {
            guard depth <= RecipeExpression.maxDepth else { return nil }
            var result: Double? = nil
            while true {
                let (value, ok) = parseAddSub(depth: depth + 1)
                if !ok { return nil }               // hard parse error
                // Only a FINITE value counts — division-by-zero (Inf) and NaN are
                // treated as "undefined" so a degenerate op is skipped, not drawn
                // at the clamped canvas boundary.
                if result == nil, let v = value, v.isFinite { result = v }
                skipSpaces()
                if peek() == "|" {
                    pos += 1
                    continue
                }
                break
            }
            return result
        }

        // Returns (value, parsedOK). value == nil means "defined-check failed"
        // (an undefined identifier); parsedOK == false means a syntax error.
        mutating func parseAddSub(depth: Int) -> (Double?, Bool) {
            guard depth <= RecipeExpression.maxDepth else { return (nil, false) }
            var (left, ok) = parseMulDiv(depth: depth + 1)
            if !ok { return (nil, false) }
            while true {
                skipSpaces()
                guard let c = peek(), c == "+" || c == "-" else { break }
                pos += 1
                let (right, rok) = parseMulDiv(depth: depth + 1)
                if !rok { return (nil, false) }
                guard let l = left, let r = right else { left = nil; continue }
                left = (c == "+") ? l + r : l - r
            }
            return (left, true)
        }

        mutating func parseMulDiv(depth: Int) -> (Double?, Bool) {
            guard depth <= RecipeExpression.maxDepth else { return (nil, false) }
            var (left, ok) = parseUnary(depth: depth + 1)
            if !ok { return (nil, false) }
            while true {
                skipSpaces()
                guard let c = peek(), c == "*" || c == "/" else { break }
                pos += 1
                let (right, rok) = parseUnary(depth: depth + 1)
                if !rok { return (nil, false) }
                guard let l = left, let r = right else { left = nil; continue }
                if c == "/" { left = (r == 0) ? nil : l / r }   // 0-divide → undefined, not Inf
                else { left = l * r }
            }
            return (left, true)
        }

        mutating func parseUnary(depth: Int) -> (Double?, Bool) {
            guard depth <= RecipeExpression.maxDepth else { return (nil, false) }
            skipSpaces()
            if peek() == "-" {
                pos += 1
                let (v, ok) = parseUnary(depth: depth + 1)
                if !ok { return (nil, false) }
                if let v { return (-v, true) }
                return (nil, true)
            }
            if peek() == "+" {
                pos += 1
                return parseUnary(depth: depth + 1)
            }
            return parsePrimary(depth: depth + 1)
        }

        mutating func parsePrimary(depth: Int) -> (Double?, Bool) {
            guard depth <= RecipeExpression.maxDepth else { return (nil, false) }
            skipSpaces()
            guard let c = peek() else { return (nil, false) }

            if c == "(" {
                pos += 1
                let value = parseCoalesce(depth: depth + 1)   // subexpr may coalesce
                skipSpaces()
                guard peek() == ")" else { return (nil, false) }
                pos += 1
                return (value, true)
            }

            if c.isNumber || c == "." {
                return parseNumber()
            }

            if c.isLetter || c == "_" {
                return parseIdentifierOrCall(depth: depth)
            }

            return (nil, false)
        }

        mutating func parseNumber() -> (Double?, Bool) {
            let start = pos
            while pos < chars.count, chars[pos].isNumber || chars[pos] == "." { pos += 1 }
            let str = String(chars[start..<pos])
            guard let d = Double(str) else { return (nil, false) }
            return (d, true)
        }

        mutating func parseIdentifierOrCall(depth: Int) -> (Double?, Bool) {
            let start = pos
            while pos < chars.count, chars[pos].isLetter || chars[pos].isNumber || chars[pos] == "_" {
                pos += 1
            }
            let name = String(chars[start..<pos])
            skipSpaces()
            if peek() == "(" {
                pos += 1
                var args: [Double?] = []
                skipSpaces()
                if peek() != ")" {
                    while true {
                        let value = parseCoalesce(depth: depth + 1)
                        args.append(value)
                        skipSpaces()
                        if peek() == "," { pos += 1; continue }
                        break
                    }
                }
                skipSpaces()
                guard peek() == ")" else { return (nil, false) }
                pos += 1
                return (applyFunction(name, args), true)
            }
            // A bare identifier resolves via the environment.
            return (resolve(name), true)
        }

        func applyFunction(_ name: String, _ args: [Double?]) -> Double? {
            switch name {
            case "sin", "cos", "tan", "sqrt", "abs", "deg", "rad":
                guard args.count == 1, let a = args[0] else { return nil }
                switch name {
                case "sin": return Foundation.sin(a)
                case "cos": return Foundation.cos(a)
                case "tan": return Foundation.tan(a)
                case "sqrt": return a < 0 ? nil : Foundation.sqrt(a)
                case "abs": return Swift.abs(a)
                case "deg": return a * .pi / 180   // degrees -> radians
                case "rad": return a * 180 / .pi   // radians -> degrees
                default: return nil
                }
            case "min", "max":
                guard args.count == 2, let a = args[0], let b = args[1] else { return nil }
                return name == "min" ? Swift.min(a, b) : Swift.max(a, b)
            default:
                return nil
            }
        }
    }
}
