#!/usr/bin/env python3
"""Provider-free structural regressions for Phase 2E product access."""

from __future__ import annotations

import json
import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]
CONTRACT = json.loads(
    (
        ROOT
        / "data"
        / "magician_v2"
        / "llm_observability"
        / "phase2-product-access-contract-v1.json"
    ).read_text(encoding="utf-8")
)
SERVICE = (
    ROOT / "magician/src/magician_v2/analytics/llm_analytics_read_service.rs"
).read_text(encoding="utf-8")
API = (ROOT / "magician-api/src/analytics_api.rs").read_text(
    encoding="utf-8"
)
PROVIDER = (
    ROOT / "magician/src/magician_v2/execution/internal_data_provider.rs"
).read_text(encoding="utf-8")
LEGACY_COMPAT = (
    ROOT / "magician/src/magician_v2/analytics/legacy_llm_compat.rs"
).read_text(encoding="utf-8")
SQL_GUARD = (
    ROOT / "magician/src/magician_v2/analytics/llm_sql_guard.rs"
).read_text(encoding="utf-8")
PACK = (
    ROOT / "magician/src/magician_v2/execution/embedded_pack_defs/internal_data.yaml"
).read_text(encoding="utf-8")
RUNTIME = (ROOT / "magician-bin/src/main.rs").read_text(encoding="utf-8")
UI = (ROOT / "ui/unified-ui/src/routes/(app)/llm/+page.svelte").read_text(
    encoding="utf-8"
)
ANALYST_TEMPLATE = (
    ROOT
    / "magician_data_v3/system/agent_templates/agents/internal-system-analyst/definition.agent.yaml"
).read_text(encoding="utf-8")
ANALYST_RUNTIME = (
    ROOT
    / "magician_data_v3/scopes/anonymous/default/agent_runtime/agents/internal-system-analyst/definition.agent.yaml"
).read_text(encoding="utf-8")


class Phase2ProductAccessContractTests(unittest.TestCase):
    def test_one_runtime_owned_reader_is_injected_into_both_consumers(self) -> None:
        self.assertEqual(CONTRACT["shared_read_owner"], "LlmAnalyticsReadService")
        self.assertIn("shared_llm_analytics_read_service", RUNTIME)
        self.assertIn(
            ".with_llm_analytics_read_service(Arc::clone(&shared_llm_analytics_read_service))",
            RUNTIME,
        )
        self.assertIn("with_llm_analytics_read_service", API)
        self.assertIn("with_llm_analytics_read_service", PROVIDER)

    def test_rest_routes_are_typed_scoped_and_do_not_accept_body_scope(self) -> None:
        for route in CONTRACT["rest_routes"]:
            path = route.split(" ", 1)[1]
            self.assertIn(f'"{path}"', RUNTIME)
        self.assertIn("resolve_scope(&req)", API)
        self.assertIn("#[serde(deny_unknown_fields)]", API)
        self.assertIn("governed_llm_request_contracts_reject_scope_and_unknown_fields", API)
        self.assertNotIn("pub principal: Option<String>", API)
        self.assertNotIn("pub workspace: Option<String>", API)

    def test_runtime_scope_precedence_is_hardened_for_the_whole_provider(self) -> None:
        self.assertIn("let params = authorize_runtime_scope(params)?;", PROVIDER)
        self.assertIn("required_runtime_scope_value(&params, \"__principal\")", PROVIDER)
        self.assertIn("required_runtime_scope_value(&params, \"__workspace\")", PROVIDER)
        self.assertIn("does not match the runtime-authorized scope", PROVIDER)
        self.assertIn("LlmScope::new(&principal, &workspace).is_valid()", PROVIDER)
        hidden_principal = PROVIDER.index('string_param(params, "__principal")')
        public_principal = PROVIDER.index('string_param(params, "principal")', hidden_principal)
        self.assertLess(hidden_principal, public_principal)

    def test_internal_actions_and_pack_contract_are_complete(self) -> None:
        for action in CONTRACT["internal_data_actions"]:
            self.assertIn(f'"{action}"', PROVIDER)
            self.assertIn(f"  {action}:", PACK)
        self.assertIn("matching_assertions_only", json.dumps(CONTRACT))
        self.assertIn("legacy_query_llm_calls_retained", json.dumps(CONTRACT))
        self.assertIn('"query_llm_calls" => query_llm_calls', PROVIDER)
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_query_storage_boundary"],
            "safe_scope_components_real_scope_directories_and_regular_parquet_files_only",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_query_external_access"],
            "disabled_after_server_owned_view_install_before_caller_sql",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_query_decode_failure"],
            "fail_closed_never_coerce_to_null",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_query_statement_gate"],
            "exactly_one_sqlparser_SELECT_or_WITH_query_before_DuckDB_prepare",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_query_body_gate"],
            "side_effect_free_SELECT_only_reject_SELECT_INTO_VALUES_TABLE_and_mutation_set_expressions",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_query_relation_gate"],
            "AST_allowlist_llm_calls_chat_session_cache_summary_and_lexically_scoped_CTEs_only",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_query_content_boundary"],
            "raw_view_absent_reasoning_summary_omitted_free_form_error_reduced_to_fixed_presence_marker_and_malformed_response_kind_redacted",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["shared_fact_legacy_content_boundary"],
            "free_form_error_reduced_to_fixed_presence_marker_malformed_response_kind_redacted_and_blank_call_identity_normalized_to_null",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_direct_parquet_policy"],
            "analyst_instructions_forbid_direct_llm_calls_scans_because_historical_partitions_can_precede_the_content_boundary",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["legacy_rest_execution"],
            "bounded_guard_connection_interrupt_before_outer_timeout_and_four_megabyte_single_or_batch_response_limit",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["generic_analytics_rest_execution"],
            "single_side_effect_free_SELECT_AST_gate_bounded_guard_external_access_disabled_interrupt_and_four_megabyte_result_limit",
        )
        self.assertEqual(
            CONTRACT["compatibility"]["adjacent_memory_analytics_execution"],
            "single_side_effect_free_SELECT_AST_gate_and_external_access_disabled_after_server_owned_view_install",
        )
        self.assertIn("regular_llm_call_partition_files", PROVIDER)
        self.assertIn("disable_internal_external_access(&conn, \"query_llm_calls\")", PROVIDER)
        self.assertIn(
            "disable_llm_query_external_access(&conn, \"llm_calls_query\")", API
        )
        self.assertIn(
            "disable_llm_query_external_access(&conn, \"llm_calls_query_batch\")",
            API,
        )
        self.assertIn("column {i} decode error", API)
        self.assertIn("column {idx} decode error", PROVIDER)
        self.assertNotIn(
            "row.get_ref(idx).unwrap_or(DuckValueRef::Null)", PROVIDER
        )
        self.assertIn("Parser::parse_sql(&DuckDbDialect {}, sql)", SQL_GUARD)
        self.assertIn("is_one_read_only_select_statement(sql)", API)
        self.assertIn(
            "fn legacy_analytics_sql_requires_exactly_one_parsed_query()", API
        )
        self.assertIn("LLM_CALLS_QUERY_TIMEOUT_SECS", API)
        self.assertIn("ANALYTICS_QUERY_TIMEOUT_SECS", API)
        self.assertIn('disable_llm_query_external_access(&conn, "analytics_query")', API)
        self.assertIn("ANALYTICS_DUCKDB_MAX_RESULT_BYTES", API)
        self.assertIn("fn bounded_query_response", API)
        self.assertIn("serde_json::to_vec(&response)", API)
        self.assertIn(
            "fn legacy_analytics_query_fails_closed_on_oversized_results()", API
        )
        self.assertIn(
            "fn analytics_query_budget_measures_the_complete_serialized_response()",
            API,
        )
        self.assertIn("push_bounded_query_batch_item", API)
        self.assertIn(
            "fn analytics_query_batches_enforce_one_aggregate_serialized_byte_limit()",
            API,
        )
        self.assertGreaterEqual(API.count("push_bounded_query_batch_item("), 3)
        self.assertIn("run_analytics_query_with_interrupt_timeout(", API)
        self.assertIn(
            'disable_llm_query_external_access(&conn, "memory_events_query")', API
        )
        self.assertIn(
            'disable_llm_query_external_access(&conn, "memory_events_query_batch")', API
        )
        self.assertIn(
            'disable_internal_external_access(&conn, "query_events")', PROVIDER
        )
        self.assertIn("disable_internal_external_access(&conn, action)", PROVIDER)
        self.assertIn(
            "fn memory_events_query_disables_external_functions_after_view_install()",
            API,
        )
        self.assertNotIn(
            "row.get(i).unwrap_or(duckdb::types::Value::Null)", API
        )
        self.assertIn("ensure_real_scoped_directory_chain", API + PROVIDER)
        self.assertIn("validate_legacy_llm_query(&sql)", API + PROVIDER)
        self.assertIn("is_read_only_select_query", SERVICE + LEGACY_COMPAT)
        self.assertIn("SetExpr::Insert(_)", SQL_GUARD)
        self.assertIn("select.into.is_none()", SQL_GUARD)
        self.assertIn("impl Visitor for ReadOnlyQueryVisitor", SQL_GUARD)
        self.assertIn("WITH payload AS (VALUES (1))", SQL_GUARD + SERVICE + LEGACY_COMPAT)
        self.assertIn("SELECT * INTO temporary_llm_copy", SERVICE + LEGACY_COMPAT)
        self.assertIn("impl Visitor for LegacyLlmSqlVisitor", LEGACY_COMPAT)
        self.assertIn("ALLOWED_RELATIONS", LEGACY_COMPAT)
        self.assertIn("legacy_error_redacted", LEGACY_COMPAT + API)
        self.assertIn("invalid_category_redacted", LEGACY_COMPAT + SERVICE)
        self.assertIn(
            "fn shared_fact_view_redacts_legacy_content_and_normalizes_blank_call_identity()",
            SERVICE,
        )
        self.assertNotIn("CREATE OR REPLACE VIEW llm_calls_raw", API + PROVIDER + LEGACY_COMPAT)
        self.assertIn(
            "fn compatibility_view_omits_reasoning_and_redacts_free_form_error()",
            LEGACY_COMPAT,
        )
        self.assertIn("Defaults are a schema-evolution device", LEGACY_COMPAT)
        self.assertIn("assert_eq!(cost, None);", LEGACY_COMPAT)
        compatibility_projection_start = LEGACY_COMPAT.index(
            "fn compatibility_projection_expr("
        )
        compatibility_projection_end = LEGACY_COMPAT.index(
            "\nfn escape_sql_literal", compatibility_projection_start
        )
        compatibility_projection = LEGACY_COMPAT[
            compatibility_projection_start:compatibility_projection_end
        ]
        self.assertIn("CAST({name} AS {ty}) AS {name}", compatibility_projection)
        self.assertNotIn(
            "COALESCE(CAST({name} AS {ty})", compatibility_projection
        )
        self.assertIn("SELECT reasoning_summary FROM llm_calls", API + PROVIDER + LEGACY_COMPAT)
        for analyst in (ANALYST_TEMPLATE, ANALYST_RUNTIME):
            self.assertIn(
                "Never use direct\n    `duckdb.read_parquet` against `analytics/llm_calls`",
                analyst,
            )
            self.assertIn(
                "historical partitions\n    can contain pre-policy restricted columns",
                analyst,
            )
        self.assertIn(
            "governed_analytics_rejects_scope_components_that_path_projection_would_rewrite",
            API,
        )
        self.assertIn(
            "legacy_llm_call_partition_discovery_rejects_symlinked_inputs",
            PROVIDER,
        )
        self.assertIn(
            "crew_health_llm_rollup_rejects_unsafe_scope_without_touching_storage",
            API,
        )

    def test_common_envelope_and_overview_denominators_are_service_owned(self) -> None:
        self.assertIn("pub struct LlmReadEnvelope<T>", SERVICE)
        for field in CONTRACT["common_envelope"]:
            self.assertIn(f"pub {field}:", SERVICE)
        for timing in CONTRACT["overview"]["timing_decomposition"]:
            self.assertIn(timing, SERVICE)
        self.assertIn("contract_validation_attempted = true", SERVICE)
        self.assertIn("sum(missing_record_count)", SERVICE)
        self.assertIn("pub captured_fact_revisions: u64", SERVICE)
        self.assertIn("pub pricing_versions: Vec<String>", SERVICE)
        self.assertIn("pub cost_sources: Vec<String>", SERVICE)
        self.assertIn("pub usage_observed_calls: u64", SERVICE)
        self.assertIn("pub cost_observed_calls: u64", SERVICE)
        self.assertEqual(
            CONTRACT["overview"]["usage_coverage"],
            "usage_observed_calls_preserves_missing_versus_reported_zero",
        )
        self.assertEqual(
            CONTRACT["overview"]["cost_coverage"],
            "cost_observed_calls_preserves_unknown_versus_legitimate_zero",
        )
        self.assertEqual(
            CONTRACT["overview"]["numeric_integrity"],
            "finite_nonnegative_overview_aggregates_and_nonfinite_fact_SQL_outputs_fail_closed",
        )
        self.assertGreaterEqual(
            SERVICE.count("run_analytics_query_with_interrupt_timeout("),
            5,
            "source installation, overview, scalar, optional, and row reads must all be bounded",
        )
        self.assertIn(
            "governed_aggregates_reject_negative_values_instead_of_inventing_zero",
            SERVICE,
        )
        self.assertIn("checked_nonnegative_f64(total_cost_usd", SERVICE)
        self.assertIn("governed_json_decode_rejects_nonfinite_values", SERVICE)
        self.assertIn(
            "overview_and_fact_sql_fail_closed_on_invalid_floating_point_values",
            SERVICE,
        )
        self.assertNotIn("u64::try_from(value).unwrap_or_default()", SERVICE)
        self.assertNotIn("json_u64(row.get(calls_index)).unwrap_or(0)", API)
        self.assertNotIn("json_f64(row.get(spend_index)).unwrap_or(0.0)", API)
        self.assertIn("crew health rollup has an invalid call count", API)
        self.assertIn("crew health rollup has invalid spend", API)
        self.assertIn("COUNT(cost_usd) AS cost_observed_calls", API)
        self.assertIn("crew health rollup has invalid cost coverage", API)
        self.assertIn("(0.0..=1.0).contains(value)", API)
        self.assertEqual(
            CONTRACT["crew_health_cost_coverage"],
            "cost_observed_calls_is_exposed_beside_seven_day_spend",
        )
        overview_handler = API.split("pub async fn llm_observability_overview_handler", 1)[1]
        overview_handler = overview_handler.split("pub async fn llm_fact_catalog_handler", 1)[0]
        self.assertNotIn("sum(cost_usd)", overview_handler.lower())

    def test_fact_sql_is_ast_validated_registry_only_and_external_access_disabled(self) -> None:
        self.assertEqual(
            CONTRACT["fact_sql"]["query_body_gate"],
            "side_effect_free_SELECT_only_reject_SELECT_INTO_VALUES_TABLE_and_mutation_set_expressions",
        )
        self.assertEqual(
            CONTRACT["fact_sql"]["bounded_relation_namespace"],
            "server_reserved_and_CTE_shadowing_rejected_before_relation_rewrite",
        )
        self.assertIn("parse_analytics_sql(sql)", SERVICE)
        self.assertIn("Parser::new(&DuckDbDialect {})", SQL_GUARD)
        self.assertIn(
            ".with_recursion_limit(ANALYTICS_SQL_RECURSION_LIMIT)", SQL_GUARD
        )
        self.assertIn("impl Visitor for FactSqlVisitor", SERVICE)
        self.assertIn("self.registry.resolve_name(&normalized)", SERVICE)
        self.assertIn("TableFactor::Table { args: Some(_), .. }", SERVICE)
        self.assertIn("cte_scopes: Vec<HashSet<String>>", SERVICE)
        self.assertIn("may not shadow an LLM fact relation", SERVICE)
        self.assertIn("is_reserved_fact_relation_name", SERVICE)
        self.assertIn("__llm_fact_bounded_llm_calls AS (SELECT 1)", SERVICE)
        self.assertIn("SET enable_external_access = false", SERVICE)
        self.assertIn("AS __llm_fact_result LIMIT", SERVICE)
        self.assertIn("serde_json::to_vec(&envelope)", SERVICE)
        self.assertIn("fact_sql_uses_ast_relation_and_table_function_allowlists", SERVICE)
        self.assertIn("fact_sql_executes_only_over_governed_in_memory_views", SERVICE)
        self.assertIn("fact_sql_reports_truncation_without_inventing_a_total", SERVICE)

    def test_llm_page_consumes_canonical_overview_without_reimplementing_formulas(self) -> None:
        self.assertIn("/api/magician/v2/analytics/llm/overview", UI)
        self.assertIn("Canonical capture health", UI)
        self.assertIn("provider_attempts", UI)
        self.assertIn("validation_attempted_calls", UI)
        self.assertIn("capture_gaps", UI)
        self.assertIn("usage_observed_calls", UI)
        self.assertIn("cost_observed_calls", UI)

    def test_authored_regression_matrix_covers_parity_scope_and_absence(self) -> None:
        required = (
            "common_envelopes_report_real_scope_range_coverage_and_pagination",
            "common_envelope_enforces_exact_serialized_response_bound",
            "call_and_attempt_detail_envelopes_fail_closed_to_selected_scope",
            "governed_llm_overview_requires_headers_and_returns_common_envelope",
            "governed_llm_detail_is_scope_bound_and_absence_is_not_found",
            "runtime_scope_is_required_authoritative_and_cross_scope_overrides_fail",
            "runtime_provider_rejects_unscoped_actions_before_dispatch",
            "internal_llm_overview_uses_the_shared_reader_envelope_without_formula_drift",
            "legacy_llm_query_connection_disables_external_table_functions_after_view_install",
            "legacy_llm_rest_query_disables_external_functions_after_view_install",
            "legacy_llm_rest_rejects_raw_catalog_and_external_relations_before_storage",
            "compatibility_sql_is_ast_allowlisted",
            "compatibility_view_omits_reasoning_and_redacts_free_form_error",
        )
        combined = SERVICE + API + PROVIDER + LEGACY_COMPAT
        for name in required:
            with self.subTest(test=name):
                self.assertIn(f"fn {name}", combined)


if __name__ == "__main__":
    unittest.main()
