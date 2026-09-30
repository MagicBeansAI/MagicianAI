//! Characterization of the current sent-index contract. No routing change.

use chrono::{TimeZone, Utc};

use super::sent_index::{SendBinding, SentMessage, SentMessageIndex};
use super::SendIdentifier;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::DeliveryScope;

fn t(hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, hour, 0, 0)
        .single()
        .expect("instant")
}

fn scope() -> DeliveryScope {
    DeliveryScope::new("alpha", "prod")
}

fn sent(act: &str, audience: &[&str]) -> SentMessage {
    SentMessage {
        act_ref: act.to_string(),
        audience: audience.iter().map(|s| s.to_string()).collect(),
        sent_at: t(9),
    }
}

#[test]
fn restart_reopens_the_same_jsonl_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().to_path_buf();
    let identifier = SendIdentifier::MessageId("orig-1@agentmail.to".into());
    SentMessageIndex::new(ArtifactV2Workspace::new(&path))
        .record(
            &scope(),
            "agentmail",
            &identifier,
            &sent("act-1", &["a@b.test"]),
            t(10),
        )
        .unwrap();
    let reopened = SentMessageIndex::new(ArtifactV2Workspace::new(&path));
    match reopened.lookup(&scope(), "agentmail", &identifier).unwrap() {
        SendBinding::Bound(found) => assert_eq!(found.act_ref, "act-1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn scopes_do_not_leak() {
    let tmp = tempfile::tempdir().unwrap();
    let index = SentMessageIndex::new(ArtifactV2Workspace::new(tmp.path()));
    let identifier = SendIdentifier::MessageId("orig-1@agentmail.to".into());
    index
        .record(
            &scope(),
            "agentmail",
            &identifier,
            &sent("act-1", &["a@b.test"]),
            t(10),
        )
        .unwrap();
    assert_eq!(
        index
            .lookup(
                &DeliveryScope::new("other", "prod"),
                "agentmail",
                &identifier
            )
            .unwrap(),
        SendBinding::Absent
    );
}

#[test]
fn corrupt_terminated_interior_line_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let layout = ArtifactV2Workspace::new(tmp.path());
    let index = SentMessageIndex::new(layout.clone());
    let identifier = SendIdentifier::MessageId("orig-1@agentmail.to".into());
    index
        .record(
            &scope(),
            "agentmail",
            &identifier,
            &sent("act-1", &["a@b.test"]),
            t(10),
        )
        .unwrap();
    let dir = layout
        .scope_root("alpha", "prod")
        .join("delivery")
        .join("index")
        .join("sent");
    let file = std::fs::read_dir(&dir)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut body = std::fs::read_to_string(&file).unwrap();
    body.push_str("this is not json\n");
    std::fs::write(&file, body).unwrap();
    assert!(index.lookup(&scope(), "agentmail", &identifier).is_err());
}

#[test]
fn export_import_round_trips_the_current_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let index = SentMessageIndex::new(ArtifactV2Workspace::new(tmp.path()));
    let identifier = SendIdentifier::MessageId("orig-1@agentmail.to".into());
    index
        .record(
            &scope(),
            "agentmail",
            &identifier,
            &sent("act-1", &["A@B.Test"]),
            t(10),
        )
        .unwrap();
    let dump = index.export_scope(&scope()).unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    let other = SentMessageIndex::new(ArtifactV2Workspace::new(tmp2.path()));
    other.import_scope(&scope(), &dump).unwrap();
    match other.lookup(&scope(), "agentmail", &identifier).unwrap() {
        SendBinding::Bound(found) => {
            assert_eq!(found.act_ref, "act-1");
            assert_eq!(found.audience, vec!["a@b.test".to_string()]);
        },
        other => panic!("{other:?}"),
    }
}
