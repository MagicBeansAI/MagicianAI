//! One VibeDev project's run list, a page at a time.
//!
//! The cockpit used to load every VibeDev task in the scope (all projects,
//! every turn) and fold them in the browser. This pages on the server: a
//! *run* is a root task plus the threaded follow-up turns folded into it (the
//! same rule as the rail — a `vibedev-threaded` turn whose `Parent task:` is
//! present), newest run first. Each page carries its runs' turns, so the rail
//! can fold and total costs for exactly what it shows.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::magician_v2::artifact_v2::models::TaskListItemV3;
use crate::magician_v2::execution::compiled_handlers::run_coding_task::parent_task_id_from_description;
use crate::magician_v2::vibedev::projects::{parse_vibedev_project_id, VibeDevRunLink};
use crate::magician_v2::vibedev::run_service::VIBEDEV_RUN_THREADED_TAG;

/// Runs per page when the client does not say.
pub const DEFAULT_RUN_PAGE_SIZE: usize = 10;
/// Most runs one page may ask for.
pub const MAX_RUN_PAGE_SIZE: usize = 50;

/// One page of runs: each root followed by its folded turns.
#[derive(Debug, Clone)]
pub struct RunPage {
    pub tasks: Vec<TaskListItemV3>,
    /// Root runs on this page.
    pub runs: usize,
    /// Root runs in the whole project (that match the search, when one is given).
    pub total_runs: usize,
    /// Index of this page's first run among `total_runs`.
    pub offset: usize,
    pub has_more: bool,
    /// Pass back as `cursor` for the next (older) page.
    pub next_cursor: Option<String>,
    /// The typed links (parent, crew, lane) of this page's tasks that have one.
    pub links: BTreeMap<String, VibeDevRunLink>,
}

fn is_threaded(item: &TaskListItemV3) -> bool {
    item.tags
        .iter()
        .any(|tag| tag.name == VIBEDEV_RUN_THREADED_TAG)
}

fn cursor_of(item: &TaskListItemV3) -> String {
    format!("{}|{}", item.updated_at, item.id)
}

/// Split `items` into run roots and, per root id, the threaded turns folded
/// into it. The run a task belongs to is found by walking threaded turns up
/// to the nearest task that is not folded (an orphaned threaded turn is its
/// own run). A turn's parent is its typed `links` entry when it has one, else
/// its `Parent task:` description line. Roots keep the input order.
pub(crate) fn fold_into_runs<'a>(
    items: &'a [TaskListItemV3],
    links: &BTreeMap<String, VibeDevRunLink>,
) -> (
    Vec<&'a TaskListItemV3>,
    HashMap<String, Vec<&'a TaskListItemV3>>,
) {
    let by_id: HashMap<&str, &TaskListItemV3> =
        items.iter().map(|item| (item.id.as_str(), item)).collect();
    let parent_of = |item: &TaskListItemV3| -> Option<String> {
        links
            .get(&item.id)
            .and_then(|link| link.parent_task_id.clone())
            .or_else(|| parent_task_id_from_description(&item.description))
    };
    let root_of = |item: &TaskListItemV3| -> String {
        let mut current = item;
        let mut seen = HashSet::new();
        while is_threaded(current) && seen.insert(current.id.clone()) {
            match parent_of(current).and_then(|parent| by_id.get(parent.as_str()).copied()) {
                Some(parent) => current = parent,
                None => break,
            }
        }
        current.id.clone()
    };

    let mut turns: HashMap<String, Vec<&'a TaskListItemV3>> = HashMap::new();
    let mut roots: Vec<&'a TaskListItemV3> = Vec::new();
    for item in items {
        let root = root_of(item);
        if root == item.id {
            roots.push(item);
        } else {
            turns.entry(root).or_default().push(item);
        }
    }
    (roots, turns)
}

/// Page `items` (a project's tasks, any order) into runs.
///
/// Items whose `VibeDev project:` line names another project are dropped
/// (the rail's `isForProject`). `cursor` is a previous page's `next_cursor`:
/// the page starts strictly after that run.
pub fn page_runs(
    items: Vec<TaskListItemV3>,
    project_id: &str,
    limit: usize,
    cursor: Option<&str>,
) -> RunPage {
    page_runs_matching(items, &BTreeMap::new(), project_id, limit, cursor, 0, "")
}

/// [`page_runs`] with a numbered page and a search.
///
/// `search` (case-insensitive) keeps the runs whose root or any folded turn
/// has it in its title or description; `total_runs` then counts only those.
/// `offset` skips that many runs and applies only when `cursor` is absent.
pub fn page_runs_matching(
    items: Vec<TaskListItemV3>,
    links: &BTreeMap<String, VibeDevRunLink>,
    project_id: &str,
    limit: usize,
    cursor: Option<&str>,
    offset: usize,
    search: &str,
) -> RunPage {
    let limit = limit.clamp(1, MAX_RUN_PAGE_SIZE);
    let needle = search.trim().to_lowercase();
    let items: Vec<TaskListItemV3> = items
        .into_iter()
        .filter(|item| {
            parse_vibedev_project_id(&item.description).is_none_or(|owner| owner == project_id)
        })
        .collect();
    let (mut roots, turns) = fold_into_runs(&items, links);
    if !needle.is_empty() {
        let matches = |item: &TaskListItemV3| {
            item.title.to_lowercase().contains(&needle)
                || item.description.to_lowercase().contains(&needle)
        };
        roots.retain(|root| {
            matches(*root)
                || turns
                    .get(&root.id)
                    .is_some_and(|folded| folded.iter().any(|turn| matches(*turn)))
        });
    }
    // Newest run first, as the rail sorts; id breaks ties so the cursor is total.
    roots.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    let total_runs = roots.len();
    let start = cursor
        .and_then(|cursor| {
            let (updated_at, id) = cursor.split_once('|')?;
            Some(
                roots
                    .iter()
                    .position(|root| {
                        (root.updated_at.as_str(), root.id.as_str()) < (updated_at, id)
                    })
                    .unwrap_or(roots.len()),
            )
        })
        .unwrap_or(offset.min(roots.len()));
    let page: Vec<&TaskListItemV3> = roots.iter().skip(start).take(limit).copied().collect();
    let has_more = start + page.len() < total_runs;
    let next_cursor = if has_more {
        page.last().map(|root| cursor_of(root))
    } else {
        None
    };
    let mut tasks = Vec::new();
    for root in &page {
        tasks.push((*root).clone());
        if let Some(folded) = turns.get(&root.id) {
            tasks.extend(folded.iter().map(|turn| (*turn).clone()));
        }
    }
    let links = tasks
        .iter()
        .filter_map(|task| {
            links
                .get(&task.id)
                .map(|link| (task.id.clone(), link.clone()))
        })
        .collect();
    RunPage {
        links,
        runs: page.len(),
        tasks,
        total_runs,
        offset: start,
        has_more,
        next_cursor,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(id: &str, updated_at: &str, parent: Option<&str>, threaded: bool) -> TaskListItemV3 {
        let mut description = String::from("VibeDev coding request:\nVibeDev project: proj\n");
        if let Some(parent) = parent {
            description.push_str(&format!("Parent task: {parent}\n"));
        }
        let tags = if threaded {
            json!([{ "id": "t", "name": VIBEDEV_RUN_THREADED_TAG }])
        } else {
            json!([])
        };
        serde_json::from_value(json!({
            "id": id, "title": id, "description": description, "status": "completed",
            "agent_id": "cto", "latest_root_execution_id": null,
            "last_completed_root_execution_id": null, "completion_summary": null,
            "completion_outcome": null,
            "tags": tags, "created_at": updated_at, "updated_at": updated_at,
        }))
        .expect("task list item")
    }

    fn ids(page: &RunPage) -> Vec<&str> {
        page.tasks.iter().map(|task| task.id.as_str()).collect()
    }

    #[test]
    fn folds_threaded_turns_into_their_run_and_pages_newest_first() {
        let items = vec![
            task("a", "2026-10-01T00:00:00Z", None, false),
            task("a2", "2026-10-01T01:00:00Z", Some("a"), true),
            task("b", "2026-10-02T00:00:00Z", None, false),
            task("c", "2026-10-03T00:00:00Z", None, false),
            // A Run-button follow-up is not threaded: its own run.
            task("c-run", "2026-10-04T00:00:00Z", Some("c"), false),
        ];
        let first = page_runs(items.clone(), "proj", 2, None);
        assert_eq!(ids(&first), vec!["c-run", "c"]);
        assert_eq!((first.total_runs, first.has_more), (4, true));

        let second = page_runs(items, "proj", 2, first.next_cursor.as_deref());
        assert_eq!(ids(&second), vec!["b", "a", "a2"]);
        assert!(!second.has_more);
        assert_eq!(second.next_cursor, None);
    }

    #[test]
    fn numbered_pages_and_search_cover_every_run_including_folded_turns() {
        let mut items = vec![
            task("a", "2026-10-01T00:00:00Z", None, false),
            task("a2", "2026-10-01T01:00:00Z", Some("a"), true),
            task("b", "2026-10-02T00:00:00Z", None, false),
            task("c", "2026-10-03T00:00:00Z", None, false),
        ];
        items[1].title = "Fix the Login form".into();
        let second = page_runs_matching(items.clone(), &BTreeMap::new(), "proj", 2, None, 2, "");
        assert_eq!(ids(&second), vec!["a", "a2"]);
        assert_eq!(
            (second.offset, second.total_runs, second.has_more),
            (2, 3, false)
        );

        // A match in a folded turn returns its whole run.
        let found = page_runs_matching(
            items.clone(),
            &BTreeMap::new(),
            "proj",
            10,
            None,
            0,
            "login",
        );
        assert_eq!(ids(&found), vec!["a", "a2"]);
        assert_eq!(found.total_runs, 1);

        let none = page_runs_matching(items, &BTreeMap::new(), "proj", 10, None, 0, "nowhere");
        assert_eq!((none.runs, none.total_runs), (0, 0));
    }

    #[test]
    fn an_orphaned_threaded_turn_and_other_projects() {
        let mut other = task("x", "2026-10-05T00:00:00Z", None, false);
        other.description = "VibeDev project: elsewhere\n".into();
        let items = vec![
            other,
            task("orphan", "2026-10-02T00:00:00Z", Some("gone"), true),
        ];
        let page = page_runs(items, "proj", 10, None);
        assert_eq!(ids(&page), vec!["orphan"]);
        assert_eq!(page.total_runs, 1);
    }

    #[test]
    fn a_typed_link_folds_a_turn_whose_description_names_no_parent() {
        // `b` is threaded but its description has no `Parent task:` line;
        // only the project's typed link says it continues `a`.
        let items = vec![
            task("a", "2026-10-01T00:00:00Z", None, false),
            task("b", "2026-10-01T01:00:00Z", None, true),
        ];
        let unlinked = page_runs(items.clone(), "proj", 10, None);
        assert_eq!(unlinked.runs, 2);

        let mut links = BTreeMap::new();
        links.insert(
            "b".to_string(),
            VibeDevRunLink {
                work_location: None,
                parent_task_id: Some("a".to_string()),
                crew_id: Some("crew-1".to_string()),
                lane: None,
            },
        );
        let linked = page_runs_matching(items, &links, "proj", 10, None, 0, "");
        assert_eq!(linked.runs, 1);
        assert_eq!(ids(&linked), vec!["a", "b"]);
        assert_eq!(linked.links.len(), 1);
        assert_eq!(linked.links["b"].crew_id.as_deref(), Some("crew-1"));
    }
}
