import Foundation

/// A recipe value in an op's params bag. A param may be a bare number, a string
/// (a field-ref / coalesce-chain / expression, or a plain literal like `text`),
/// or a (possibly nested) array — nested arrays hold `[x, y]` point pairs or a
/// `points` list. The decoder is deliberately flexible: unknown shapes decode as
/// best-effort so a novel recipe never crashes the client.
public enum RecipeValue: Equatable {
    case number(Double)
    case string(String)
    case array([RecipeValue])

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        // A JSON boolean (`fill: true`, `stroke: false`) decodes as 1/0 — the
        // `bool` accessor reads it back. Bool is checked before Double because
        // `Bool` decodes cleanly and shouldn't be coerced to a number by chance.
        if let b = try? container.decode(Bool.self) {
            self = .number(b ? 1 : 0)
            return
        }
        if let d = try? container.decode(Double.self) {
            self = .number(d)
            return
        }
        if let s = try? container.decode(String.self) {
            self = .string(s)
            return
        }
        if let arr = try? container.decode([RecipeValue].self) {
            self = .array(arr)
            return
        }
        throw DecodingError.dataCorruptedError(
            in: container,
            debugDescription: "RecipeValue is not a bool, number, string, or array"
        )
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .number(let d): try container.encode(d)
        case .string(let s): try container.encode(s)
        case .array(let a): try container.encode(a)
        }
    }
}

extension RecipeValue: Codable {}

/// One draw-op: an `op` name plus a flexible params bag. The reserved key `op`
/// is stripped from `params` so the bag holds only the op's arguments.
public struct RecipeOp: Equatable {
    public let op: String
    public let params: [String: RecipeValue]

    public init(op: String, params: [String: RecipeValue]) {
        self.op = op
        self.params = params
    }
}

extension RecipeOp: Codable {
    private struct DynamicKey: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
        init(_ s: String) { self.stringValue = s }
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: DynamicKey.self)
        var op = ""
        var params: [String: RecipeValue] = [:]
        for key in container.allKeys {
            if key.stringValue == "op" {
                op = (try? container.decode(String.self, forKey: key)) ?? ""
            } else {
                params[key.stringValue] = try container.decode(RecipeValue.self, forKey: key)
            }
        }
        self.op = op
        self.params = params
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: DynamicKey.self)
        try container.encode(op, forKey: DynamicKey("op"))
        for (k, v) in params {
            try container.encode(v, forKey: DynamicKey(k))
        }
    }
}

/// A declarative primitive recipe: which shape `type` (plus `aliases`) it renders,
/// optional `defaults` for absent fields, and an ordered `draw` list of ops.
public struct TutorRecipe: Codable, Equatable {
    public let type: String
    public let aliases: [String]
    public let version: Int?
    public let defaults: [String: Double]
    public let draw: [RecipeOp]

    public init(
        type: String,
        aliases: [String] = [],
        version: Int? = nil,
        defaults: [String: Double] = [:],
        draw: [RecipeOp] = []
    ) {
        self.type = type
        self.aliases = aliases
        self.version = version
        self.defaults = defaults
        self.draw = draw
    }

    private enum CodingKeys: String, CodingKey {
        case type, aliases, version, defaults, draw
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        type = try c.decode(String.self, forKey: .type)
        aliases = try c.decodeIfPresent([String].self, forKey: .aliases) ?? []
        version = try c.decodeIfPresent(Int.self, forKey: .version)
        defaults = try c.decodeIfPresent([String: Double].self, forKey: .defaults) ?? [:]
        draw = try c.decodeIfPresent([RecipeOp].self, forKey: .draw) ?? []
    }

    /// All `type` strings this recipe answers to (primary + aliases), lowercased.
    public var matchedTypes: [String] {
        ([type] + aliases).map { $0.lowercased() }
    }
}
