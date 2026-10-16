//  LTMEnums.swift
//  Live Thinking Map (LTM) — canonical wire enums.
//
//  S1a canonical wire contract. Every type here mirrors a Rust type in
//  `magician_v2::thinking_map` and round-trips the authoritative JSON fixtures
//  in `Fixtures/` losslessly.
//
//  CRITICAL: everything is namespaced under the caseless enum `LTM` to avoid
//  colliding with the E0 prototype's top-level `ThinkingNode` / `ThinkingEdge` /
//  `ThinkingMapSource` / `ThinkingMapSnapshot` in the same module.
//
//  All Rust enums are `#[serde(rename_all = "snake_case")]`, so each Swift enum
//  is a `String`-raw `Codable` enum with explicit snake_case raw values.

import Foundation

/// Caseless namespace for the Live Thinking Map canonical wire contract.
public enum LTM {}

extension LTM {
    /// `NodeKind` — first-class kinds of thinking node.
    public enum NodeKind: String, Codable, Equatable, Sendable {
        case idea
        case fact
        case question
        case decision
        case option
        case risk
        case action
        case metric
        case assumption
        case evidence
        case group
    }

    /// `EpistemicState` — how settled/true a node currently is.
    public enum EpistemicState: String, Codable, Equatable, Sendable {
        case provisional
        case asserted
        case confirmed
        case contradicted
        case rejected
        case resolved
        case superseded
    }

    /// `AssertionOrigin` — where an assertion originated.
    public enum AssertionOrigin: String, Codable, Equatable, Sendable {
        case ownerSpoken = "owner_spoken"
        case participantSpoken = "participant_spoken"
        case ownerEdited = "owner_edited"
        case importedSource = "imported_source"
        case modelInferred = "model_inferred"
        case systemDerived = "system_derived"
    }

    /// `EdgeKind` — directed relationship kinds.
    public enum EdgeKind: String, Codable, Equatable, Sendable {
        case relatedTo = "related_to"
        case supports
        case contradicts
        case answers
        case dependsOn = "depends_on"
        case leadsTo = "leads_to"
        case alternativeTo = "alternative_to"
        case measures
        case groupedUnder = "grouped_under"
    }

    /// `MapLifecycle` — map lifecycle status. Rust default = `.active`.
    public enum MapLifecycle: String, Codable, Equatable, Sendable {
        case active
        case paused
        case archived
        case deleted
    }

    /// `ViewLens` — which lens the shared view renders through. Default `.graph`.
    public enum ViewLens: String, Codable, Equatable, Sendable {
        case graph
        case mindMap = "mind_map"
        case outline
        case decision
        case metrics
    }

    /// `PromotionKind` — destination for a node promoted to another surface.
    public enum PromotionKind: String, Codable, Equatable, Sendable {
        case task
        case today
        case memory
    }

    /// `ClarificationState` — lifecycle of a clarification. Default `.open`.
    public enum ClarificationState: String, Codable, Equatable, Sendable {
        case open
        case answered
        case deferred
        case dismissed
    }

    /// `ProposalState` — lifecycle of a restructure proposal. Default `.proposed`.
    public enum ProposalState: String, Codable, Equatable, Sendable {
        case proposed
        case confirmed
        case rejected
        case deferred
    }
}
