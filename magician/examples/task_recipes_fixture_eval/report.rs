//! Report model: one row per case × phase, gates with details, and a
//! diagnostics bundle attached only where a gate failed.

use std::{collections::BTreeMap, fs, path::Path};

use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

use crate::sites::RequestRecord;

#[derive(Debug, Clone, Serialize)]
pub struct Gate {
    pub id: String,
    pub passed: bool,
    pub detail: String,
}

impl Gate {
    pub fn new(id: impl Into<String>, passed: bool, detail: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            passed,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Diagnostics {
    pub recipe: Option<Value>,
    pub recipe_events: Vec<Value>,
    pub log_lines: Vec<String>,
    pub fixture_requests: Vec<RequestRecord>,
    pub approvals: Vec<Value>,
    pub outcome_details: Option<Value>,
    /// Sessions the fixture had issued when the phase ended. Compared against
    /// a request's `cookie_sid_prefix`, this separates "replayed a session the
    /// site never issued" from "sent no session at all".
    pub site_sessions: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Phase {
    pub id: String,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub status: String,
    pub outcome_type: String,
    pub summary_excerpt: String,
    pub duration_ms: u64,
    pub gates: Vec<Gate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<Diagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Phase {
    pub fn passed(&self) -> bool {
        self.error.is_none() && self.gates.iter().all(|gate| gate.passed)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Case {
    pub id: String,
    pub site: String,
    pub origin: String,
    pub passed: bool,
    pub recipe_id: Option<String>,
    pub phases: Vec<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Case {
    pub fn finish(mut self) -> Self {
        self.passed = self.error.is_none()
            && !self.phases.is_empty()
            && self.phases.iter().all(Phase::passed);
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema_version: u32,
    pub generated_at: String,
    pub passed: bool,
    pub principal: String,
    pub workspace: String,
    pub magician_version: String,
    pub sites: BTreeMap<String, String>,
    pub cases: Vec<Case>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fatal_error: Option<String>,
}

impl Report {
    pub fn finalize(mut self) -> Self {
        self.passed = self.fatal_error.is_none()
            && !self.cases.is_empty()
            && self.cases.iter().all(|case| case.passed);
        self
    }
}

pub fn write(report: &Report, out_dir: &Path) -> Result<()> {
    fs::create_dir_all(out_dir)?;
    fs::write(
        out_dir.join("report.json"),
        serde_json::to_vec_pretty(report)?,
    )?;
    fs::write(out_dir.join("latest.html"), render_html(report))?;
    Ok(())
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

fn state(passed: bool) -> &'static str {
    if passed {
        "pass"
    } else {
        "FAIL"
    }
}

pub fn render_html(report: &Report) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        "<h1>Task Recipes fixture eval</h1><p>Passed: <strong class=\"{}\">{}</strong> · scope {}/{} · magician {} · {}</p>",
        state(report.passed),
        report.passed,
        escape(&report.principal),
        escape(&report.workspace),
        escape(&report.magician_version),
        escape(&report.generated_at)
    ));
    if let Some(error) = &report.fatal_error {
        body.push_str(&format!("<p class=\"FAIL\">Fatal: {}</p>", escape(error)));
    }
    if !report.sites.is_empty() {
        body.push_str("<p class=\"muted\">Sites: ");
        for (name, origin) in &report.sites {
            body.push_str(&format!("{} = {} · ", escape(name), escape(origin)));
        }
        body.push_str("</p>");
    }
    for case in &report.cases {
        body.push_str(&format!(
            "<h2><span class=\"{}\">{}</span> {} <span class=\"muted\">({} · {})</span></h2>",
            state(case.passed),
            state(case.passed),
            escape(&case.id),
            escape(&case.site),
            escape(case.recipe_id.as_deref().unwrap_or("no recipe"))
        ));
        if let Some(error) = &case.error {
            body.push_str(&format!("<p class=\"FAIL\">{}</p>", escape(error)));
        }
        body.push_str("<table><thead><tr><th>Phase</th><th>Task</th><th>Outcome</th><th>Gate</th><th>State</th><th>Detail</th></tr></thead><tbody>");
        for phase in &case.phases {
            let rows = phase.gates.len().max(1);
            let first = format!(
                "<td rowspan=\"{rows}\">{}<br><span class=\"muted\">{} ms</span></td><td rowspan=\"{rows}\">{}<br><span class=\"muted\">{}</span></td><td rowspan=\"{rows}\">{} · {}<br><span class=\"muted\">{}</span></td>",
                escape(&phase.id),
                phase.duration_ms,
                escape(phase.task_id.as_deref().unwrap_or("-")),
                escape(phase.execution_id.as_deref().unwrap_or("-")),
                escape(&phase.status),
                escape(&phase.outcome_type),
                escape(&phase.summary_excerpt)
            );
            if phase.gates.is_empty() {
                body.push_str(&format!(
                    "<tr>{first}<td colspan=\"3\" class=\"FAIL\">{}</td></tr>",
                    escape(phase.error.as_deref().unwrap_or("no gates evaluated"))
                ));
            }
            for (index, gate) in phase.gates.iter().enumerate() {
                body.push_str("<tr>");
                if index == 0 {
                    body.push_str(&first);
                }
                body.push_str(&format!(
                    "<td>{}</td><td class=\"{}\">{}</td><td>{}</td></tr>",
                    escape(&gate.id),
                    state(gate.passed),
                    state(gate.passed),
                    escape(&gate.detail)
                ));
            }
            if let Some(error) = &phase.error {
                if !phase.gates.is_empty() {
                    body.push_str(&format!(
                        "<tr><td colspan=\"6\" class=\"FAIL\">error: {}</td></tr>",
                        escape(error)
                    ));
                }
            }
            if let Some(diagnostics) = &phase.diagnostics {
                let json = serde_json::to_string_pretty(diagnostics).unwrap_or_default();
                body.push_str(&format!(
                    "<tr><td colspan=\"6\"><details><summary>diagnostics ({} events · {} log lines · {} fixture requests)</summary><pre>{}</pre></details></td></tr>",
                    diagnostics.recipe_events.len(),
                    diagnostics.log_lines.len(),
                    diagnostics.fixture_requests.len(),
                    escape(&json)
                ));
            }
        }
        body.push_str("</tbody></table>");
    }
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Task Recipes fixture eval</title><style>body{{font:14px system-ui;max-width:1200px;margin:40px auto;padding:0 16px}}table{{width:100%;border-collapse:collapse;margin-bottom:24px}}td,th{{padding:6px 8px;border-bottom:1px solid #ddd;text-align:left;vertical-align:top}}.pass{{color:#1a7f37}}.FAIL{{color:#b42318;font-weight:600}}.muted{{color:#666}}pre{{max-height:480px;overflow:auto;background:#f6f6f6;padding:8px}}</style></head><body>{body}</body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(id: &str, gates: Vec<Gate>) -> Phase {
        Phase {
            id: id.into(),
            task_id: None,
            execution_id: None,
            status: "completed".into(),
            outcome_type: "recipe_replay".into(),
            summary_excerpt: String::new(),
            duration_ms: 1,
            gates,
            diagnostics: None,
            error: None,
        }
    }

    fn case(id: &str, phases: Vec<Phase>) -> Case {
        Case {
            id: id.into(),
            site: "catalog".into(),
            origin: "http://127.0.0.1:1".into(),
            passed: false,
            recipe_id: None,
            phases,
            error: None,
        }
        .finish()
    }

    fn report(cases: Vec<Case>) -> Report {
        Report {
            schema_version: 1,
            generated_at: "now".into(),
            passed: false,
            principal: "p".into(),
            workspace: "w".into(),
            magician_version: "0".into(),
            sites: BTreeMap::new(),
            cases,
            fatal_error: None,
        }
        .finalize()
    }

    #[test]
    fn report_passed_requires_every_case_and_every_gate() {
        let good = case("c1", vec![phase("cold", vec![Gate::new("a", true, "")])]);
        let bad = case(
            "c2",
            vec![phase(
                "warm",
                vec![Gate::new("a", true, ""), Gate::new("b", false, "x")],
            )],
        );
        assert!(good.passed);
        assert!(!bad.passed);
        assert!(report(vec![good.clone()]).passed);
        assert!(!report(vec![good, bad]).passed);
        assert!(!report(vec![]).passed, "an empty run is not a pass");
        let mut errored = case("c3", vec![phase("cold", vec![Gate::new("a", true, "")])]);
        errored.error = Some("boom".into());
        assert!(!errored.finish().passed);
    }

    #[test]
    fn html_escapes_detail_text() {
        let evil = case(
            "c1",
            vec![phase(
                "warm",
                vec![Gate::new("g", false, "<script>alert(1)</script>")],
            )],
        );
        let html = render_html(&report(vec![evil]));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(!html.contains("<script>alert(1)</script>"));
    }
}
