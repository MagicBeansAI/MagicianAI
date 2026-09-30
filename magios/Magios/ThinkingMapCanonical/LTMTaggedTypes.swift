//  LTMTaggedTypes.swift
//  Live Thinking Map (LTM) — the three internally-tagged enums + the
//  three-state `FieldEdit` optional used by `update_node.detail_markdown`.
//
//  Rust internally-tagged enums (serde `#[serde(tag = "...")]`) put the
//  discriminant as a sibling key alongside the variant's fields. Swift has no
//  built-in equivalent, so each needs a hand-written `Codable`.
//
//  - `LTM.Source`  (`ThinkingMapSource`) — tagged on `"kind"`; `solo` has no
//    extra fields (`{"kind":"solo"}`).
//  - `LTM.Actor`   (`OperationActor`)     — tagged on `"actor"`.
//  - `LTM.Operation` — tagged on `"op"` — lives in `LTMOperation.swift`.

import Foundation

// MARK: - FieldEdit (Option<Option<T>> semantics)

extension LTM {
    /// Models Rust's `Option<Option<T>>` field-edit semantics used by
    /// `UpdateNode.detail_markdown`:
    ///   - key absent            → `.unchanged`  (leave the field as-is)
    ///   - key present == `null` → `.clear`      (`Some(None)` — clear to null)
    ///   - key present == value  → `.set(value)` (`Some(Some(x))` — set)
    ///
    /// This is decoded/encoded *manually by the containing type* using
    /// `decodeFieldEdit` / `encodeFieldEdit` below, because Swift's synthesized
    /// `Codable` cannot distinguish an absent key from a `null` value for a
    /// normal `Optional` property (both decode to `nil`). The containing type
    /// must therefore use `decodeIfPresent`-style probing and drive these
    /// helpers explicitly.
    public enum FieldEdit<T: Codable & Equatable & Sendable>: Equatable, Sendable {
        /// Field omitted from the wire — leave unchanged (`None`).
        case unchanged
        /// Field present as explicit JSON `null` — clear (`Some(None)`).
        case clear
        /// Field present with a value — set (`Some(Some(value))`).
        case set(T)
    }
}

extension KeyedDecodingContainer {
    /// Decode a `FieldEdit` for `key`, distinguishing absent vs null vs value.
    func decodeFieldEdit<T>(_ key: Key) throws -> LTM.FieldEdit<T> {
        guard contains(key) else { return .unchanged }
        // Present. Could be an explicit `null` (→ clear) or a value (→ set).
        if try decodeNil(forKey: key) {
            return .clear
        }
        let value = try decode(T.self, forKey: key)
        return .set(value)
    }
}

extension KeyedEncodingContainer {
    /// Encode a `FieldEdit` for `key` mirroring serde's
    /// `skip_serializing_if = "Option::is_none"` on the outer `Option`:
    ///   - `.unchanged` → omit the key entirely
    ///   - `.clear`     → encode explicit `null`
    ///   - `.set(v)`    → encode `v`
    mutating func encodeFieldEdit<T>(_ edit: LTM.FieldEdit<T>, forKey key: Key) throws {
        switch edit {
        case .unchanged:
            break // omit
        case .clear:
            try encodeNil(forKey: key)
        case let .set(value):
            try encode(value, forKey: key)
        }
    }
}

// MARK: - Source (ThinkingMapSource, tagged on "kind")

extension LTM {
    /// `ThinkingMapSource` — internally tagged on `"kind"`. `solo` carries no
    /// payload (`{"kind":"solo"}`); the rest carry a single string field.
    public enum Source: Codable, Equatable, Sendable {
        case solo
        case meeting(threadId: String)
        case observe(sessionId: String)
        case chat(threadId: String)
        case imported(sourceKind: String)
        case tutor(lessonId: String)

        private enum CodingKeys: String, CodingKey {
            case kind
            case threadId = "thread_id"
            case sessionId = "session_id"
            case sourceKind = "source_kind"
            case lessonId = "lesson_id"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let kind = try c.decode(String.self, forKey: .kind)
            switch kind {
            case "solo":
                self = .solo
            case "meeting":
                self = .meeting(threadId: try c.decode(String.self, forKey: .threadId))
            case "observe":
                self = .observe(sessionId: try c.decode(String.self, forKey: .sessionId))
            case "chat":
                self = .chat(threadId: try c.decode(String.self, forKey: .threadId))
            case "imported":
                self = .imported(sourceKind: try c.decode(String.self, forKey: .sourceKind))
            case "tutor":
                self = .tutor(lessonId: try c.decode(String.self, forKey: .lessonId))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .kind, in: c,
                    debugDescription: "unknown ThinkingMapSource kind: \(kind)")
            }
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case .solo:
                try c.encode("solo", forKey: .kind)
            case let .meeting(threadId):
                try c.encode("meeting", forKey: .kind)
                try c.encode(threadId, forKey: .threadId)
            case let .observe(sessionId):
                try c.encode("observe", forKey: .kind)
                try c.encode(sessionId, forKey: .sessionId)
            case let .chat(threadId):
                try c.encode("chat", forKey: .kind)
                try c.encode(threadId, forKey: .threadId)
            case let .imported(sourceKind):
                try c.encode("imported", forKey: .kind)
                try c.encode(sourceKind, forKey: .sourceKind)
            case let .tutor(lessonId):
                try c.encode("tutor", forKey: .kind)
                try c.encode(lessonId, forKey: .lessonId)
            }
        }
    }
}

// MARK: - Actor (OperationActor, tagged on "actor")

extension LTM {
    /// `OperationActor` — internally tagged on `"actor"`. The `model` variant's
    /// `trace_id` is `Option<String>` with `skip_serializing_if` → it's absent
    /// when nil (mirrored here via `decodeIfPresent` / conditional encode).
    public enum Actor: Codable, Equatable, Sendable {
        case owner(principal: String)
        case participant(speakerId: String)
        case model(traceId: String?)
        case trustedSystem(component: String)
        case imported(sourceKind: String)

        private enum CodingKeys: String, CodingKey {
            case actor
            case principal
            case speakerId = "speaker_id"
            case traceId = "trace_id"
            case component
            case sourceKind = "source_kind"
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let tag = try c.decode(String.self, forKey: .actor)
            switch tag {
            case "owner":
                self = .owner(principal: try c.decode(String.self, forKey: .principal))
            case "participant":
                self = .participant(speakerId: try c.decode(String.self, forKey: .speakerId))
            case "model":
                self = .model(traceId: try c.decodeIfPresent(String.self, forKey: .traceId))
            case "trusted_system":
                self = .trustedSystem(component: try c.decode(String.self, forKey: .component))
            case "imported":
                self = .imported(sourceKind: try c.decode(String.self, forKey: .sourceKind))
            default:
                throw DecodingError.dataCorruptedError(
                    forKey: .actor, in: c,
                    debugDescription: "unknown OperationActor actor: \(tag)")
            }
        }

        public func encode(to encoder: Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            switch self {
            case let .owner(principal):
                try c.encode("owner", forKey: .actor)
                try c.encode(principal, forKey: .principal)
            case let .participant(speakerId):
                try c.encode("participant", forKey: .actor)
                try c.encode(speakerId, forKey: .speakerId)
            case let .model(traceId):
                try c.encode("model", forKey: .actor)
                // skip_serializing_if = Option::is_none → only encode when set.
                try c.encodeIfPresent(traceId, forKey: .traceId)
            case let .trustedSystem(component):
                try c.encode("trusted_system", forKey: .actor)
                try c.encode(component, forKey: .component)
            case let .imported(sourceKind):
                try c.encode("imported", forKey: .actor)
                try c.encode(sourceKind, forKey: .sourceKind)
            }
        }
    }
}
