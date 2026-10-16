//! Owned-tab inventory header for the browser inner-loop prompt.
//!
//! On every iteration, the runner asks the dispatcher for an
//! `iteration_header` (see `super::super::runner::PrimitiveDispatcher`).
//! For the browser pack we surface the per-execution owned-tab list
//! that `magicutor::server::cdp_proxy` maintains: Tier-1 tabs in the
//! dedicated window plus Tier-2 popups whose opener chain leads back
//! to ours. The model uses this to know what tabs exist, which one
//! is current, and whether a popup it should engage with has appeared
//! mid-flow — without having to call a discovery tool.
//!
//! ## Why we go through the agent-browser CLI
//!
//! Earlier drafts of this file fetched the inventory through a
//! dedicated magicutor HTTP endpoint and rendered Chrome's numeric
//! tab ids plus opener lineage. We removed that endpoint after a
//! design pass: the agent's own `tab` tool surfaces (Phase 3) call
//! `agent-browser tab list` which goes through the same proxy and
//! returns agent-browser's `t1`/`t2` labels — so having the runtime
//! header speak a different identifier shape (`tab-1328863302`) than
//! the agent's tools (`t1`) was just confusing.
//!
//! The cost: agent-browser's `tab list --json` doesn't surface
//! `openerTabId`, so the header can't render `opened_by [N]` lineage.
//! For most pages this isn't critical; the agent can still see the
//! popup, decide whether to engage, and switch via `agent-browser
//! tab t<N>`. If we ever need lineage in the header, the right move
//! is to extend agent-browser's `tab list` schema, not to re-add the
//! HTTP endpoint.
//!
//! Header shape (example):
//!
//! ```text
//! ## Open tabs in this execution
//! - [0] t1 url=https://www.makemytrip.com/flight/search?... title="MakeMyTrip" current
//! - [1] t2 url=https://www.makemytrip.com/ title="MakeMyTrip - …"
//! ```
//!
//! See `docs/plans/2026-05-03-browser-pack-target-ownership-and-tabs.md`
//! for the full design and the staged revert path.

use std::sync::Arc;

use serde::Deserialize;
use tracing::debug;

use super::session::AgentBrowserSession;

/// Cap on rows in the rendered header. Past the cap entries get
/// summarized as `(+ N more)`. Keeps prompt cost bounded even on
/// tasks that spawn many popups.
const MAX_HEADER_ROWS: usize = 8;
/// Cap on chars per URL/title field; longer values are truncated with
/// an ellipsis to keep each row to one line.
const MAX_FIELD_CHARS: usize = 80;

/// One row of agent-browser's `tab list --json` output. Mirrors the
/// CLI schema (`{tabId: "t1", label, title, type, url, active}`).
/// Extra fields are tolerated via serde defaults so the parser
/// survives agent-browser version drift.
#[derive(Debug, Clone, Deserialize)]
pub struct OwnedTabInventoryEntry {
    pub tab_id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub active: bool,
}

#[derive(Debug, Deserialize)]
struct TabListResponse {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    data: Option<TabListData>,
}

#[derive(Debug, Deserialize)]
struct TabListData {
    #[serde(default)]
    tabs: Vec<TabListEntry>,
}

#[derive(Debug, Deserialize)]
struct TabListEntry {
    #[serde(rename = "tabId")]
    tab_id: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    active: bool,
}

/// Build the inventory header text for `session`. Returns `None` when
/// agent-browser's `tab list` returns no tabs, when the CLI call
/// fails, or when the JSON shape isn't what we expect (defensive —
/// the header is best-effort and must never block prompt rendering).
pub async fn render_owned_tabs_header(session: &Arc<AgentBrowserSession>) -> Option<String> {
    let tabs = fetch_inventory(session).await?;
    format_owned_tabs_header(&tabs)
}

async fn fetch_inventory(
    session: &Arc<AgentBrowserSession>,
) -> Option<Vec<OwnedTabInventoryEntry>> {
    let result = match session.run_command(&["tab", "list", "--json"]).await {
        Ok(result) => result,
        Err(err) => {
            debug!(
                session = session.session_id(),
                error = %err,
                "owned_tabs_header: agent-browser tab list spawn failed; skipping"
            );
            return None;
        },
    };
    if !result.success {
        debug!(
            session = session.session_id(),
            stderr = result.stderr.trim(),
            "owned_tabs_header: agent-browser tab list returned non-zero; skipping"
        );
        return None;
    }
    let parsed: TabListResponse = match serde_json::from_str(result.stdout.trim()) {
        Ok(parsed) => parsed,
        Err(err) => {
            debug!(
                session = session.session_id(),
                error = %err,
                "owned_tabs_header: tab list JSON parse failed; skipping"
            );
            return None;
        },
    };
    if !parsed.success {
        return None;
    }
    let tabs = parsed
        .data?
        .tabs
        .into_iter()
        .map(|entry| OwnedTabInventoryEntry {
            tab_id: entry.tab_id,
            label: entry.label,
            title: entry.title,
            url: entry.url,
            active: entry.active,
        })
        .collect::<Vec<_>>();
    Some(tabs)
}

/// Render the inventory rows. Public so unit tests can exercise the
/// formatting without standing up an agent-browser session.
pub fn format_owned_tabs_header(tabs: &[OwnedTabInventoryEntry]) -> Option<String> {
    if tabs.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(tabs.len().min(MAX_HEADER_ROWS) + 2);
    lines.push("## Open tabs in this execution".to_string());
    let visible = tabs.len().min(MAX_HEADER_ROWS);
    for (idx, tab) in tabs.iter().take(visible).enumerate() {
        lines.push(format_row(idx, tab));
    }
    if tabs.len() > visible {
        lines.push(format!("- (+ {} more)", tabs.len() - visible));
    }
    Some(lines.join("\n"))
}

fn format_row(idx: usize, tab: &OwnedTabInventoryEntry) -> String {
    let mut parts = vec![format!("[{idx}]"), tab.tab_id.clone()];
    if let Some(label) = tab
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("label={}", label));
    }
    if let Some(url) = tab.url.as_deref() {
        parts.push(format!("url={}", truncate_field(url)));
    }
    if let Some(title) = tab
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        parts.push(format!("title=\"{}\"", truncate_field(title)));
    }
    if tab.active {
        parts.push("current".to_string());
    }
    format!("- {}", parts.join(" "))
}

fn truncate_field(value: &str) -> String {
    let collapsed: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_FIELD_CHARS {
        return collapsed;
    }
    let mut truncated: String = collapsed.chars().take(MAX_FIELD_CHARS).collect();
    truncated.push('…');
    truncated
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn entry(tab_id: &str, url: &str, title: &str, active: bool) -> OwnedTabInventoryEntry {
        OwnedTabInventoryEntry {
            tab_id: tab_id.to_string(),
            label: None,
            title: Some(title.to_string()),
            url: Some(url.to_string()),
            active,
        }
    }

    #[test]
    fn format_owned_tabs_header_returns_none_for_empty_inventory() {
        assert!(format_owned_tabs_header(&[]).is_none());
    }

    #[test]
    fn format_owned_tabs_header_renders_single_active_tab() {
        let header = format_owned_tabs_header(&[entry(
            "t1",
            "https://www.makemytrip.com/flight/search?from=BLR&to=BBI",
            "MakeMyTrip",
            true,
        )])
        .expect("header");
        assert!(header.starts_with("## Open tabs in this execution"));
        assert!(header.contains("[0] t1"));
        assert!(header.contains("title=\"MakeMyTrip\""));
        assert!(header.contains("current"));
    }

    #[test]
    fn format_owned_tabs_header_renders_label_when_present() {
        let mut tab = entry("t2", "https://docs.example.com/", "Docs", false);
        tab.label = Some("docs".to_string());
        let header = format_owned_tabs_header(&[tab]).expect("header");
        assert!(header.contains("label=docs"));
    }

    #[test]
    fn format_owned_tabs_header_renders_multiple_tabs_with_indices() {
        let header = format_owned_tabs_header(&[
            entry(
                "t1",
                "https://www.makemytrip.com/flight/search?...",
                "MakeMyTrip",
                true,
            ),
            entry(
                "t2",
                "https://www.makemytrip.com/",
                "MakeMyTrip - homepage",
                false,
            ),
        ])
        .expect("header");
        assert!(header.contains("[0] t1"));
        assert!(header.contains("[1] t2"));
        assert!(header.contains("current"));
    }

    #[test]
    fn format_owned_tabs_header_caps_rows_and_summarizes_overflow() {
        let mut tabs = Vec::new();
        for idx in 0..(MAX_HEADER_ROWS + 5) {
            tabs.push(entry(
                &format!("t{}", idx + 1),
                &format!("https://example.com/{idx}"),
                &format!("page-{idx}"),
                idx == 0,
            ));
        }
        let header = format_owned_tabs_header(&tabs).expect("header");
        assert!(header.contains("(+ 5 more)"));
        let visible_rows = header
            .lines()
            .filter(|line| line.starts_with("- ["))
            .count();
        assert_eq!(visible_rows, MAX_HEADER_ROWS);
    }

    #[test]
    fn format_owned_tabs_header_truncates_long_urls() {
        let long_url = format!("https://example.com/{}", "a".repeat(200));
        let header =
            format_owned_tabs_header(&[entry("t1", &long_url, "page", true)]).expect("header");
        let row = header
            .lines()
            .find(|line| line.starts_with("- ["))
            .expect("row");
        assert!(
            row.contains('…'),
            "long url should be truncated with ellipsis"
        );
        assert!(
            row.chars().count() < long_url.len(),
            "row must be shorter than full url"
        );
    }

    #[test]
    fn format_owned_tabs_header_skips_empty_title_field() {
        let mut tab = entry("t7", "https://example.com", "", true);
        tab.title = Some("   ".to_string());
        let header = format_owned_tabs_header(&[tab]).expect("header");
        assert!(!header.contains("title=\"\""));
        assert!(!header.contains("title=\"   \""));
    }

    #[test]
    fn parses_real_agent_browser_tab_list_payload() {
        let raw = r#"{"success":true,"data":{"tabs":[{"active":true,"label":null,"tabId":"t1","title":"Test page","type":"page","url":"http://localhost:5173/x.html"}]},"error":null}"#;
        let parsed: TabListResponse = serde_json::from_str(raw).expect("parses");
        assert!(parsed.success);
        let tabs = parsed.data.expect("data").tabs;
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].tab_id, "t1");
        assert_eq!(tabs[0].title.as_deref(), Some("Test page"));
        assert!(tabs[0].active);
    }
}
