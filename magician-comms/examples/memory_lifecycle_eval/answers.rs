use super::*;

pub async fn run(root: &Path, router: &OperationLlmRouter, fixture: &Value) -> Result<Value> {
    fs::create_dir(root)?;
    let layout = ArtifactV2Workspace::new(root);
    let resolver = AgentMemoryResolver::with_workspace_layout(layout.clone());
    let service = resolver.resolve_for_scope("answer-owner", "answer-workspace")?;
    let now = chrono::Utc::now();
    service.save_user_knowledge(&json!({"preferences":[{"key":"exercise_preference",
        "value":fixture["earlier"],"source_type":"explicit_user_statement",
        "updated_at":(now-chrono::Duration::days(60)).to_rfc3339(),"memory_review_version":lifecycle::POLICY_VERSION}]})).await?;
    let fields = serde_json::Map::from_iter([(
        "exercise_pattern".into(),
        json!({"value":fixture["observed"],"source_type":"observed_behavior"}),
    )]);
    let saved = merge_user_memory_tier_fields(
        &resolver,
        "answer-owner",
        "answer-workspace",
        "preferences",
        &fields,
    )
    .await;
    ensure!(saved["status"] == "ok", "could not save observation");
    // A restart must tear down timeout/publication tasks as well as the service:
    // they correctly retain the durable writer lease while they can still write.
    let phase =
        std::thread::scope(|scope| {
            scope.spawn(|| {
        tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(async {
    let requests = request_service(root, layout.clone()).await;
    let mut observations = Vec::new();
    let first = runtime::pass(
        &service,
        Some(router),
        Some(&requests),
        now,
        Some(&mut |o| observations.push(observe(o))),
    )
    .await?;
    let before = service.load_user_knowledge().await?;
    let conflict: Option<lifecycle::Conflict> = before[lifecycle::JOURNAL]["conflicts"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
        .filter_map(|v| serde_json::from_value::<lifecycle::Conflict>(v.clone()).ok())
        .find(|c| c.state == "pending");
    let Some(conflict) = conflict else {
        fs::write(root.join("failed-initial-review.json"), serde_json::to_vec_pretty(
            &json!({"first":first,"document":before,"observations":observations}))?)?;
        anyhow::bail!("expected owner clarification; initial evidence retained");
    };
    let id = runtime::request_id("answer-owner", "answer-workspace", &conflict.id);
    let answer = requests
        .respond_scoped(
            UserResponse {
                request_id: id,
                decision: "answer".into(),
                input: fixture["answer"].as_str().map(str::to_owned),
                channel: "evaluation".into(),
                sensitive: Vec::new(),
            },
            Some("answer-owner"),
            Some("answer-workspace"),
        )
        .await;
    ensure!(
        matches!(answer, ScopedResponseResult::Accepted),
        "free-text answer not accepted"
    );
    Ok::<_, anyhow::Error>((first, before, conflict, observations))
        })
    }).join().expect("clarification phase panicked")
        })?;
    let (first, before, conflict, mut observations) = phase;
    let requests = request_service(root, layout).await;
    let second = runtime::pass(
        &service,
        Some(router),
        Some(&requests),
        now + chrono::Duration::seconds(1),
        Some(&mut |o| observations.push(observe(o))),
    )
    .await?;
    let after = service.load_user_knowledge().await?;
    let recalled = magician::magician_v2::attention::resurfacing::memory_connections::recall(
        &service,
        "Exercise timing during school holidays",
    )
    .await?;
    let recalled_text = recalled
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let qualified = fixture["must_recall"]
        .as_array()
        .unwrap()
        .iter()
        .all(|word| recalled_text.contains(word.as_str().unwrap()));
    let resolved = after[lifecycle::JOURNAL]["conflicts"][&conflict.id]["state"] == "resolved";
    runtime::reconcile_questions(&service, &requests, now + chrono::Duration::seconds(2)).await?;
    let replay_safe = after == service.load_user_knowledge().await?;
    let passed = first.applied == 1
        && second.applied == 1
        && first.error.is_none()
        && second.error.is_none()
        && resolved
        && qualified
        && replay_safe;
    Ok(
        json!({"passed":passed,"fixture":fixture,"first":first,"second":second,"resolved":resolved,
        "qualified_recall":qualified,"replay_safe":replay_safe,"before":before,"after":after,"recall":recalled,"observations":observations}),
    )
}
