//  LTMResponses.swift
//  Live Thinking Map (LTM) — the HTTP response shapes + shared coder config.
//
//  The operations/interpret/patch handlers in
//  `magician_v2::api::thinking_maps_api` emit one of three outcome bodies,
//  discriminated by the `"outcome"` key:
//    - `{"outcome":"applied","resulting_revision":N,"semantic_hash":"...","map":{...}}`
//    - `{"outcome":"idempotent_replay","resulting_revision":N}`
//    - `{"outcome":"no_operations"}`
//  `LTM.ApplyOutcome` is a manual `Codable` keyed on `"outcome"`.

import Foundation

extension LTM {
    /// The apply/interpret/patch response body. Manual `Codable` on `"outcome"`.
    public enum ApplyOutcome: Codable, Equatable, Sendable {
        /// `applied` — the envelope changed the map; the full updated map is
        /// returned alongside the new revision + semantic hash.
        case applied(resultingRevision: UInt64, semanticHash: String, map: Map)
        /// `idempotent_replay` — the envelope was a replay; nothing changed.
        case idempotentReplay(resultingRevision: UInt64)
        /// `no_operations` — a valid zero-move interpretation (interpret only).
        case noOperations

        private enum CodingKeys: String, CodingKey {
            case outcome
            case resultingRevision = "resulting_revision"
            case semanticHash = "semantic_hash"
            case map
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let outcome = try c.decode(String.self, forKey: .outcome)
            switch outcome {
            case "applied":
                self = .applied(
                    resultingRevision: try c.decode(UInt64.self, forKey: .resultingRevision),
                    semanticHash: try c.decode(String.self, forKey: .semanticHash),
                    map: try c.decode(Map.self, forKey: .map)
                )
            case "idempotent_replay":
                self = .idempotentReplay(
                    resultingRevision: try c.decode(UInt64.self, forKey: .resultingRevision))
            case "no_operations":
                self = .noOperations
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .outcome, in: c,
                    debugDescription: "unknown ApplyOutcome outcome: \(outcome)")
            }
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .applied(resultingRevision, semanticHash, map):
                try c.encode("applied", forKey: .outcome)
                try c.encode(resultingRevision, forKey: .resultingRevision)
                try c.encode(semanticHash, forKey: .semanticHash)
                try c.encode(map, forKey: .map)
            case let .idempotentReplay(resultingRevision):
                try c.encode("idempotent_replay", forKey: .outcome)
                try c.encode(resultingRevision, forKey: .resultingRevision)
            case .noOperations:
                try c.encode("no_operations", forKey: .outcome)
            }
        }
    }
}

// MARK: - Shared coder configuration

extension LTM {
    /// The ONE canonical wire coder configuration. Because every `LTM` type uses
    /// explicit snake_case `CodingKeys` (and the internally-tagged enums need
    /// manual `Codable` regardless), the encoder/decoder use NO key-conversion
    /// strategy — the raw values already carry the exact wire names. Mixing in
    /// `.convertToSnakeCase` would be redundant AND would corrupt the manual
    /// enums' discriminant/field keys, so it is deliberately NOT used.
    public enum Wire {
        /// Shared decoder for canonical JSON.
        public static func makeDecoder() -> JSONDecoder {
            JSONDecoder()
        }

        /// Shared encoder for canonical JSON.
        public static func makeEncoder() -> JSONEncoder {
            let encoder = JSONEncoder()
            return encoder
        }
    }
}
