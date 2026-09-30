//  LTMInterpretIntent.swift
//  Live Thinking Map (LTM) — the `/interpret` steering intent.
//
//  Mirrors the Rust `InterpretIntent` and the `intent` request field parsed by
//  `magician_v2::api::thinking_maps_api::parse_intent`. The wire values are the
//  snake_case strings `"continue_thinking"` / `"break_open"`; the server treats
//  an absent/unknown value as `continue_thinking` (the safe default).
//
//  Lives in a NEW file (not added to the S1a wire files) so S1a stays untouched.

import Foundation

extension LTM {
    /// `InterpretIntent` — steering intent for `/interpret`. Raw values are the
    /// exact request-body strings.
    public enum InterpretIntent: String, Codable, Equatable, Sendable {
        /// Extend the current line of thinking (server default).
        case continueThinking = "continue_thinking"
        /// Deliberately open up / diverge from the current framing.
        case breakOpen = "break_open"
    }
}
