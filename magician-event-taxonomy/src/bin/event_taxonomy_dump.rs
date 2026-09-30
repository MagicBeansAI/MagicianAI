//! Codegen binary — emits the TS mirror of `RuntimeTransportEvent::taxonomy()`.
//!
//! Reads `magician_event_taxonomy::EVENT_TAXONOMY_TABLE` (single source
//! of truth, generated from the same `taxonomies!` macro that backs the
//! runtime match) and writes a Svelte-friendly TS file to stdout.
//!
//! Wired into `make event-taxonomy-codegen`, which redirects stdout to
//! `ui/unified-ui/src/lib/realtime/event-taxonomy.ts` and then runs
//! `scripts/event_taxonomy_check.py stamp …` to embed a SHA-256 of this
//! binary's source plus `realtime_events.rs` as a `// SOURCE_HASH:`
//! marker.
//!
//! `make test`, `build-all-debug`, and `build-all-release` no longer run
//! the codegen directly (it took 3–8 minutes on a cold `target/`). They
//! run the fast `make event-taxonomy-check` gate instead, which compares
//! the embedded marker against a freshly-computed source hash and fails
//! loudly if they diverge — at which point the operator runs
//! `make event-taxonomy-codegen` and commits the regenerated file.

use magician_event_taxonomy::{
    EventCategory, EventSeverity, EVENT_TAXONOMY_TABLE, GAUI_EVENT_TAXONOMY,
};

fn main() {
    let mut out = String::new();
    out.push_str(HEADER);
    out.push_str("\nexport const EVENT_CATEGORIES = [\n");
    for category in [
        EventCategory::Pipeline,
        EventCategory::Plan,
        EventCategory::Tool,
        EventCategory::Slot,
        EventCategory::Clarification,
        EventCategory::Execution,
        EventCategory::Llm,
        EventCategory::Agentic,
        EventCategory::Hitl,
        EventCategory::Agent,
        EventCategory::Task,
        EventCategory::Feed,
        EventCategory::Observability,
        EventCategory::Media,
        EventCategory::Activity,
    ] {
        out.push_str(&format!("\t'{}',\n", category_str(category)));
    }
    out.push_str("] as const;\n");
    out.push_str(CATEGORY_TYPE);

    out.push_str("\nexport const EVENT_SEVERITIES = [");
    for severity in [
        EventSeverity::Info,
        EventSeverity::Warn,
        EventSeverity::Error,
        EventSeverity::Decision,
        EventSeverity::Attention,
    ] {
        out.push_str(&format!("'{}', ", severity_str(severity)));
    }
    out.truncate(out.trim_end_matches(", ").len());
    out.push_str("] as const;\n");
    out.push_str(SEVERITY_TYPE);

    out.push_str(TAXONOMY_INTERFACE);

    out.push_str("\nexport const EVENT_TAXONOMY: Readonly<Record<string, EventTaxonomy>> = Object.freeze({\n");

    let mut last_category: Option<EventCategory> = None;
    for (variant, taxonomy) in EVENT_TAXONOMY_TABLE {
        // Insert a blank line between category groups for readability.
        if last_category != Some(taxonomy.category) {
            if last_category.is_some() {
                out.push('\n');
            }
            out.push_str(&format!(
                "\t// ─── {} ───\n",
                category_label(taxonomy.category)
            ));
            last_category = Some(taxonomy.category);
        }
        out.push_str(&format!(
            "\t{}: t('{}', '{}', {}),\n",
            variant,
            category_str(taxonomy.category),
            severity_str(taxonomy.severity),
            taxonomy.user_relevant
        ));
    }
    // GAUI envelope event_types — dot-namespaced strings emitted via
    // `RuntimeTransportBroadcaster::emit_scoped_or_unscoped` and wrapped
    // in `RuntimeTransportEvent::AgentEvent`. Without these rows in the
    // TS mirror, the `/events` UI's `taxonomyFor()` falls back to the
    // catch-all `observability/info/false` for every llm.* / tool.* /
    // execution.* / agentic.* / step.* / etc. event, hiding the
    // granular category/severity carried server-side.
    //
    // Renamed from AGUI to GAUI in magician v0.6.485 — formerly only
    // covered the chat-path (tool.call.*, reasoning.*, plan.snapshot,
    // agent.cycle.*); now covers the full `ArtifactV2EventType`
    // canonical event registry plus the surviving legacy AGUI strings.
    out.push_str("\n\t// ─── GAUI envelope events ───\n");
    let mut last_category: Option<EventCategory> = None;
    for (event_type, taxonomy) in GAUI_EVENT_TAXONOMY {
        if last_category != Some(taxonomy.category) {
            if last_category.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("\t// {}\n", category_label(taxonomy.category)));
            last_category = Some(taxonomy.category);
        }
        // Quote the event_type because dot-separated identifiers like
        // `tool.call.started` are not valid bare object keys.
        out.push_str(&format!(
            "\t'{}': t('{}', '{}', {}),\n",
            event_type,
            category_str(taxonomy.category),
            severity_str(taxonomy.severity),
            taxonomy.user_relevant
        ));
    }
    out.push_str("});\n");

    out.push_str(FOOTER);

    print!("{}", out);
}

fn category_str(c: EventCategory) -> &'static str {
    match c {
        EventCategory::Pipeline => "pipeline",
        EventCategory::Plan => "plan",
        EventCategory::Tool => "tool",
        EventCategory::Slot => "slot",
        EventCategory::Clarification => "clarification",
        EventCategory::Execution => "execution",
        EventCategory::Llm => "llm",
        EventCategory::Agentic => "agentic",
        EventCategory::Hitl => "hitl",
        EventCategory::Agent => "agent",
        EventCategory::Task => "task",
        EventCategory::Feed => "feed",
        EventCategory::Observability => "observability",
        EventCategory::Media => "media",
        EventCategory::Activity => "activity",
    }
}

fn category_label(c: EventCategory) -> &'static str {
    match c {
        EventCategory::Pipeline => "Pipeline",
        EventCategory::Plan => "Plan",
        EventCategory::Tool => "Tool matching",
        EventCategory::Slot => "Slot extraction / parameter resolution",
        EventCategory::Clarification => "Clarification (planning HITL)",
        EventCategory::Execution => "Execution + Workflow lifecycle",
        EventCategory::Llm => "LLM I/O",
        EventCategory::Agentic => "Agentic loop",
        EventCategory::Hitl => "HITL (execution-side AskUser / confirmation)",
        EventCategory::Agent => "Agent lifecycle",
        EventCategory::Task => "Task CRUD",
        EventCategory::Feed => "Feed",
        EventCategory::Observability => "Observability (catch-all)",
        EventCategory::Media => "Realtime media + control rails",
        EventCategory::Activity => "Activity (unified runtime activity spans)",
    }
}

fn severity_str(s: EventSeverity) -> &'static str {
    match s {
        EventSeverity::Info => "info",
        EventSeverity::Warn => "warn",
        EventSeverity::Error => "error",
        EventSeverity::Decision => "decision",
        EventSeverity::Attention => "attention",
    }
}

const HEADER: &str = "/**
 * Event taxonomy — TS mirror of `magician/src/magician_v2/realtime_events.rs`
 * `RuntimeTransportEvent::taxonomy()`.
 *
 * GENERATED FILE — DO NOT EDIT BY HAND.
 *
 * Run `make event-taxonomy-codegen` to regenerate from the Rust source
 * (the `taxonomies!` macro invocation in realtime_events.rs is the single
 * source of truth). `make test`, `build-all-debug`, and `build-all-release`
 * run the fast `make event-taxonomy-check` gate as a prerequisite — it
 * verifies the `// SOURCE_HASH:` marker below still matches a freshly
 * computed hash of the Rust sources and fails the build with a clear
 * remediation message if codegen needs to be re-run.
 */
";

const CATEGORY_TYPE: &str = "
export type EventCategory = (typeof EVENT_CATEGORIES)[number];
";

const SEVERITY_TYPE: &str = "
export type EventSeverity = (typeof EVENT_SEVERITIES)[number];
";

const TAXONOMY_INTERFACE: &str = "
export interface EventTaxonomy {
\tcategory: EventCategory;
\tseverity: EventSeverity;
\tuser_relevant: boolean;
}

const t = (
\tcategory: EventCategory,
\tseverity: EventSeverity,
\tuser_relevant: boolean
): EventTaxonomy => ({ category, severity, user_relevant });
";

const FOOTER: &str = "
/** Lookup with safe fallback for events that haven't been categorised yet. */
export function taxonomyFor(eventType: string): EventTaxonomy {
\treturn (
\t\tEVENT_TAXONOMY[eventType] ?? {
\t\t\tcategory: 'observability',
\t\t\tseverity: 'info',
\t\t\tuser_relevant: false
\t\t}
\t);
}

/** All event_type strings that exist in the taxonomy. Useful for filter chip options. */
export const KNOWN_EVENT_TYPES: readonly string[] = Object.freeze(Object.keys(EVENT_TAXONOMY));
";
