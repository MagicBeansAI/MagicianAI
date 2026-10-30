use magician::magician_v2::{
    context_retrieval::progress::{
        ContextPreparationProgress, ContextPreparationTiming, ContextProgressCorrelation,
    },
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};
use std::sync::Arc;

#[tokio::test]
async fn decision_model_receipts_persist_once_with_usage_without_execution_status_authority() {
    use decision_engine_contract::telemetry::{DecisionCallStatus, DecisionModelCall};
    use magician::magician_v2::{
        analytics::decision_model_telemetry,
        artifact_v2::events::{
            map_v2_realtime_event, ArtifactV2EventType, CanonicalEventScope,
            RuntimeCanonicalEventSink,
        },
        decision_host,
    };
    #[derive(Default)]
    struct Sink(std::sync::Mutex<Vec<serde_json::Value>>);
    impl RuntimeCanonicalEventSink for Sink {
        fn emit(
            &self,
            scope: CanonicalEventScope,
            kind: ArtifactV2EventType,
            payload: serde_json::Value,
        ) {
            assert_eq!(scope.task_id, "model-task");
            assert_eq!(kind, ArtifactV2EventType::LlmSucceeded);
            self.0.lock().unwrap().push(payload);
        }
    }
    let bus = Arc::new(RuntimeTransportBroadcaster::new(16));
    let sink = Arc::new(Sink::default());
    bus.set_runtime_canonical_event_sink(sink.clone());
    bus.register_runtime_canonical_event_scope(CanonicalEventScope {
        principal: "model-owner".into(),
        workspace: "default".into(),
        task_id: "model-task".into(),
        execution_id: "model-execution".into(),
        ui_thread_id: "general".into(),
    });
    decision_host::set_health_broadcaster(&bus);
    let mut events = bus.subscribe();
    let mut scope = magicllm::LlmTraceContext::new(
        magicllm::LlmScope::new("model-owner", "default"),
        magicllm::LlmWorkloadClass::InteractiveTask,
    );
    scope.task_id = Some("model-task".into());
    scope.execution_id = Some("model-execution".into());
    let mut call = DecisionModelCall {
        batch_id: None,
        item_ids: Vec::new(),
        call_id: "model-call".into(),
        retry_group_id: "model-group".into(),
        attempt: 1,
        operation: "tool_action_judge".into(),
        adapter: "kev-onnx".into(),
        provider: "decision:kev-onnx".into(),
        requested_model: "local-model".into(),
        model: "local-model".into(),
        local: true,
        started_at_ms: 1_790_467_200_000,
        completed_at_ms: 1_790_467_200_080,
        latency_ms: 80,
        queue_wait_ms: 0,
        status: DecisionCallStatus::Succeeded,
        error_class: None,
        input_tokens: Some(1200),
        output_tokens: Some(0),
        cache_read_tokens: None,
        cache_write_tokens: None,
    };
    let mut event = decision_model_telemetry::event(&scope, &call);
    let mapped = map_v2_realtime_event(&event).unwrap();
    assert_eq!(mapped.execution_status_hint, None);
    assert_eq!(mapped.task_status_hint, None);
    // Ordinary model events retain their existing state hints.
    if let RuntimeTransportEvent::LLMResponseReceived { response_kind, .. } = &mut event {
        *response_kind = "llm".into();
    }
    assert_eq!(
        map_v2_realtime_event(&event).unwrap().task_status_hint,
        Some("running")
    );
    decision_model_telemetry::record(&scope, &[call.clone()], Some("model-agent"));
    {
        let recorded = sink.0.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0]["input_tokens"], 1200);
        assert_eq!(recorded[0]["output_tokens"], 0);
        assert!(recorded[0]["cache_read_tokens"].is_null());
    }
    assert!(events.try_recv().is_ok());
    assert!(
        events.try_recv().is_err(),
        "one transport receipt per physical call"
    );
    call.call_id = "unmeasured-model-call".into();
    call.input_tokens = None;
    call.output_tokens = None;
    decision_model_telemetry::record(&scope, &[call.clone()], Some("model-agent"));
    {
        let recorded = sink.0.lock().unwrap();
        assert_eq!(recorded.len(), 2);
        assert!(recorded[1]["input_tokens"].is_null());
        assert!(recorded[1]["output_tokens"].is_null());
    }
    let _ = events.try_recv().unwrap();
    scope.execution_id = Some("unregistered-execution".into());
    decision_model_telemetry::record(&scope, &[call], Some("model-agent"));
    assert_eq!(sink.0.lock().unwrap().len(), 2, "no synthetic task scope");
    assert!(events.try_recv().is_ok());
}

#[tokio::test]
async fn incomplete_context_stages_do_not_claim_zero_search_time() {
    use magician::magician_v2::context_retrieval::{
        ContextRetrievalCheckpoint, ContextRetrievalObservation, ContextStageKind,
        ContextStageState,
    };
    let mut observation = ContextRetrievalObservation::setup_timed_out(
        ContextRetrievalCheckpoint::ChatTurn,
        5_000,
        5_000.0,
    );
    for stage in &mut observation.stages {
        stage.elapsed_ms = 0.0;
        if stage.stage == ContextStageKind::ReusableProcedures {
            stage.status = ContextStageState::Completed;
            stage.elapsed_ms = 3_347.0;
        }
    }
    let timing = ContextPreparationTiming::from(&observation);
    assert!(timing.memory_incomplete);
    assert_eq!(timing.memory_ms, None);
    assert_eq!(timing.procedures_ms, Some(3_347.0));
    let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(8));
    let mut events = broadcaster.subscribe();
    let mut progress = ContextPreparationProgress::start(
        Some(broadcaster),
        Some("owner"),
        Some("default"),
        "agent",
        ContextProgressCorrelation {
            chat_session_id: Some("chat".into()),
            chat_turn_id: Some("turn".into()),
            ..Default::default()
        },
    )
    .unwrap();
    progress.finish(&timing);
    let _ = events.try_recv().unwrap();
    let RuntimeTransportEvent::AgentEvent { event } = events.try_recv().unwrap() else {
        panic!("completion event");
    };
    assert_eq!(event.payload["duration_ms"], 5_000.0);
    assert!(event.payload["timing"]["memory_ms"].is_null());
    let summary = event.payload["summary"].as_str().unwrap();
    assert!(summary.contains("Memory search: not completed"));
    assert!(summary.contains("Procedures: 3347 ms"));
    assert!(!summary.contains("Memory search: 0 ms"));
}

#[tokio::test]
async fn task_context_progress_is_canonical_and_refuses_a_mismatched_registered_scope() {
    use magician::magician_v2::artifact_v2::events::{
        ArtifactV2EventType, CanonicalEventScope, RuntimeCanonicalEventSink,
    };
    #[derive(Default)]
    struct Sink(
        std::sync::Mutex<Vec<(CanonicalEventScope, ArtifactV2EventType, serde_json::Value)>>,
    );
    impl RuntimeCanonicalEventSink for Sink {
        fn emit(
            &self,
            scope: CanonicalEventScope,
            kind: ArtifactV2EventType,
            payload: serde_json::Value,
        ) {
            self.0.lock().unwrap().push((scope, kind, payload));
        }
    }
    let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
    let sink = Arc::new(Sink::default());
    broadcaster.set_runtime_canonical_event_sink(sink.clone());
    broadcaster.register_runtime_canonical_event_scope(CanonicalEventScope {
        principal: "fixture-owner".into(),
        workspace: "default".into(),
        task_id: "fixture-task".into(),
        execution_id: "fixture-execution".into(),
        ui_thread_id: "general".into(),
    });
    let mut events = broadcaster.subscribe();
    for owner in ["fixture-owner", "another-owner"] {
        let mut progress = ContextPreparationProgress::start(
            Some(broadcaster.clone()),
            Some(owner),
            Some("default"),
            "fixture-agent",
            ContextProgressCorrelation {
                task_id: Some("fixture-task".into()),
                execution_id: Some("fixture-execution".into()),
                ..Default::default()
            },
        )
        .unwrap();
        progress.finish(&ContextPreparationTiming::default());
    }
    let recorded = sink.0.lock().unwrap();
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[0].1, ArtifactV2EventType::ContextRetrievalStarted);
    assert_eq!(
        recorded[1].1,
        ArtifactV2EventType::ContextRetrievalCompleted
    );
    assert_eq!(recorded[0].2["phase_id"], recorded[1].2["phase_id"]);
    assert_eq!(recorded[1].0.task_id, "fixture-task");
    assert!(events.try_recv().is_ok());
    assert!(events.try_recv().is_ok());
    assert!(
        events.try_recv().is_err(),
        "mismatched scope is never published"
    );
}

#[tokio::test]
async fn context_progress_pairs_completion_or_cancellation_with_the_same_scoped_phase() {
    let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(16));
    let mut events = broadcaster.subscribe();
    for cancel in [false, true] {
        let mut progress = ContextPreparationProgress::start(
            Some(broadcaster.clone()),
            Some("fixture-owner"),
            Some("fixture-workspace"),
            "fixture-agent",
            ContextProgressCorrelation {
                chat_session_id: Some("fixture-chat".into()),
                chat_turn_id: Some("fixture-turn".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let RuntimeTransportEvent::AgentEvent { event: started } = events.try_recv().unwrap()
        else {
            panic!("start event")
        };
        assert_eq!(started.event_type, "context.retrieval.started");
        assert_eq!(started.principal.as_deref(), Some("fixture-owner"));
        assert_eq!(started.workspace.as_deref(), Some("fixture-workspace"));
        if !cancel {
            progress.finish(&ContextPreparationTiming {
                duration_ms: 120.0,
                memory_ms: Some(100.0),
                procedures_ms: Some(40.0),
                partial: true,
                ..Default::default()
            });
        }
        drop(progress);
        let RuntimeTransportEvent::AgentEvent { event: finished } = events.try_recv().unwrap()
        else {
            panic!("finish event")
        };
        assert_eq!(finished.event_type, "context.retrieval.completed");
        assert_eq!(finished.payload["phase_id"], started.payload["phase_id"]);
        assert_eq!(finished.payload["chat_turn_id"], "fixture-turn");
        assert_eq!(
            finished.payload["status"],
            if cancel { "cancelled" } else { "partial" }
        );
        if !cancel {
            assert_eq!(finished.payload["duration_ms"], 120.0);
            assert!(finished.payload["summary"]
                .as_str()
                .unwrap()
                .contains("Memory search: 100 ms"));
        }
        assert!(events.try_recv().is_err(), "one terminal event per phase");
    }
    assert!(ContextPreparationProgress::start(
        Some(broadcaster),
        None,
        Some("fixture-workspace"),
        "fixture-agent",
        ContextProgressCorrelation::default()
    )
    .is_none());
    assert!(
        events.try_recv().is_err(),
        "missing authority produces no cross-scope event"
    );
}
