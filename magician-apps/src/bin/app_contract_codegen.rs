//! Generate deterministic supported-public Apps schemas, OpenAPI, and fixtures.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use magician::magician_v2::apps::component_contract::validate_app_data_plane_component_contract;
use magician::magician_v2::apps::composition_service::{
    AppActionCompositionChainProgress, AppActionCompositionRequest, AppActionCompositionResponse,
    AppActionResultComposition,
};
use magician::magician_v2::apps::models::{
    AppActionInvocation, AppActionLaunchResponse, AppActionResult, AppArtifactProjection,
    AppDataEnvelope, AppDirectActionRequest, AppErrorEnvelope, AppInstallationId,
    AppMutationCommand, AppProtocolVersion, AppQueryPage, AppQueryRequest, AppReference,
    AppRunHandle, AppRunSnapshot, AppRunStatus, ValidateAppContract,
    APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION, APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
};
use magician::magician_v2::apps::records::AppMutationReceipt;
use magician::magician_v2::apps::value_mapping::AppValueMappingOperation;
use magician::magician_v2::apps::workflows::{
    AppActionCancellationReceipt, AppActionCancellationRequest,
};
use magician_app_contract::{
    public_operation_inventory, public_operation_inventory_digest, AppContractCapabilities,
    AppContractCapabilityLimits, AppHttpMethod, AppManifestFeature, AppOperationParameterLocation,
    APP_JSON_SCHEMA_DIALECT, APP_MANIFEST_SCHEMA_VERSION,
};
use magician_apps::apps::entity_changes::{AppEntityChangeBatch, MAX_APP_ENTITY_CHANGE_LIMIT};
use magician_apps::apps::fixtures::canonical_app_contract_fixtures;
use schemars::{schema_for, JsonSchema, Schema};
use serde_json::{json, Map, Value};

const CONTRACT_DIR: &str = "docs/contracts/app-platform/v1";
const COMPONENT_CONTRACT_PATH: &str = "docs/contracts/app-platform/components/v1/contract.json";
const TYPESCRIPT_PATH: &str = "ui/unified-ui/src/lib/app-platform/AppContractFixtures.generated.ts";
const TYPESCRIPT_SDK_CONTRACT_PATH: &str = "sdk/typescript/src/generated/public-contract.ts";
const TYPESCRIPT_SDK_INTERACTIVE_PATH: &str =
    "sdk/typescript/src/generated/interactive-contract.ts";
const SWIFT_PATH: &str = "magios/Shared/AppContractFixtures.generated.swift";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Generate,
    Check,
}

#[derive(Debug)]
struct Artifact {
    relative_path: PathBuf,
    contents: Vec<u8>,
}

fn main() -> Result<()> {
    let mode = match env::args().nth(1).as_deref() {
        Some("generate") | None => Mode::Generate,
        Some("check") => Mode::Check,
        Some(other) => bail!("unsupported mode `{other}`; expected `generate` or `check`"),
    };

    let manifest_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let repo_root_override = env::var_os("MAGICIAN_APP_CONTRACT_REPO_ROOT").map(PathBuf::from);
    let repo_root = match repo_root_override.as_deref() {
        Some(root) => root,
        None => manifest_root
            .parent()
            .context("magician crate must live directly under the repository root")?,
    };
    validate_immutable_component_contract(repo_root)?;
    let artifacts = build_artifacts()?;
    match mode {
        Mode::Generate => write_artifacts(repo_root, &artifacts),
        Mode::Check => check_artifacts(repo_root, &artifacts),
    }
}

fn validate_immutable_component_contract(repo_root: &Path) -> Result<()> {
    let expected = json_artifact(
        COMPONENT_CONTRACT_PATH.to_owned(),
        &json!({
            "contract_version": APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
            "fixture_digest": "blake3:a151be30e178f3b56e5aee2b47916ccd3cfd0326c820960cf78e8b7e3a0cbed0",
            "generated_clients": [
                "typescript_fixture_contract",
                "swift_fixture_contract"
            ],
            "protocol_version": "1",
            "runtime_routes_enabled": false,
            "schema_digest": "blake3:b2f2e2d9df926ae1153b2657ae29075e3df50c5285cc276a8b46733115b98dc7",
            "schema_version": 1,
            "source_of_truth": "magician/src/magician_v2/apps/models.rs",
            "supported_protocol_versions": ["1"]
        }),
    )?;
    let path = repo_root.join(COMPONENT_CONTRACT_PATH);
    let actual = fs::read(&path)
        .with_context(|| format!("reading immutable component contract {}", path.display()))?;
    validate_app_data_plane_component_contract(&actual).map_err(|error| {
        anyhow::anyhow!(
            "immutable component contract failed identity validation at {}: {error}",
            path.display()
        )
    })?;
    if actual != expected.contents {
        bail!(
            "immutable app data-plane component contract is not the canonical {} artifact: {}",
            APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
            path.display()
        );
    }
    Ok(())
}

fn build_artifacts() -> Result<Vec<Artifact>> {
    let schemas = contract_schemas()?;
    let fixtures = contract_fixtures()?;

    let schema_digest = digest_named_values(&schemas)?;
    let fixture_digest = digest_named_values(&fixtures)?;
    let protocol_version = AppProtocolVersion::V1.as_str();
    let operations = public_operation_inventory();
    let operation_inventory_digest = public_operation_inventory_digest();

    let mut artifacts = Vec::new();
    for (name, schema) in &schemas {
        artifacts.push(json_artifact(
            format!("{CONTRACT_DIR}/schemas/{name}.schema.json"),
            schema,
        )?);
    }
    for (name, fixture) in &fixtures {
        artifacts.push(json_artifact(
            format!("{CONTRACT_DIR}/fixtures/{name}.json"),
            fixture,
        )?);
    }
    artifacts.push(json_artifact(
        format!("{CONTRACT_DIR}/operations.json"),
        &serde_json::to_value(&operations).context("serializing public operation inventory")?,
    )?);

    let contract = json!({
        "schema_version": 1,
        "contract_version": APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
        "data_plane_component_contract_version": APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
        "protocol_version": protocol_version,
        "supported_protocol_versions": [protocol_version],
        "schema_digest": schema_digest,
        "fixture_digest": fixture_digest,
        "operation_inventory_digest": operation_inventory_digest,
        "source_of_truth": "magician/src/magician_v2/apps/models.rs",
        "public_contract_source": "magician-app-contract/src/lib.rs",
        "generated_clients": [
            "typescript_fixture_contract",
            "typescript_supported_public_sdk",
            "swift_fixture_contract"
        ],
        "runtime_routes_enabled": true,
        "supported_public_operation_count": operations.len()
    });
    artifacts.push(json_artifact(
        format!("{CONTRACT_DIR}/contract.json"),
        &contract,
    )?);

    let openapi =
        build_supported_public_openapi(&schemas, &schema_digest, &operation_inventory_digest)?;
    artifacts.push(json_artifact(
        format!("{CONTRACT_DIR}/openapi.json"),
        &openapi,
    )?);
    artifacts.push(text_artifact(
        TYPESCRIPT_PATH,
        render_typescript_fixture_contract(
            protocol_version,
            &schema_digest,
            &fixture_digest,
            &fixtures,
        )?,
    ));
    artifacts.push(text_artifact(
        TYPESCRIPT_SDK_CONTRACT_PATH,
        render_typescript_sdk_contract(protocol_version, &operations)?,
    ));
    artifacts.push(text_artifact(
        TYPESCRIPT_SDK_INTERACTIVE_PATH,
        render_typescript_sdk_interactive_contract(),
    ));
    artifacts.push(text_artifact(
        SWIFT_PATH,
        render_swift_fixture_contract(
            protocol_version,
            &schema_digest,
            &fixture_digest,
            &fixtures,
        )?,
    ));
    Ok(artifacts)
}

fn render_typescript_sdk_interactive_contract() -> String {
    r#"// Generated by `make app-contract-codegen`; do not edit independently.

export const APP_INTERACTIVE_CAPABILITY_REQUEST_SCHEMA =
  "magician.app-interactive-capability-request.v1" as const;

export type AppInteractiveOwnerKind = "browser" | "macos" | "android";
export type AppInteractiveTargetProfileClass =
  | "installation_ephemeral_headless"
  | "owner_reviewed_macos_pairing"
  | "owner_reviewed_android_pairing";
export type AppInteractiveActionClass =
  | "observe"
  | "navigate_or_launch"
  | "interact"
  | "capture_pixels"
  | "transfer_artifact"
  | "outward_commit";
export type AppInteractiveBackgroundPosture = "direct_owner" | "reviewed_bounded_background";
export type AppInteractiveCapturePosture = "structured_evidence_only" | "reviewed_pixels";
export type AppInteractiveTransferPosture = "denied" | "reviewed_artifacts";
export type AppInteractiveSessionPosture = "invocation_bound" | "run_bound";

export interface AppInteractiveTargetSelectors {
  readonly bundle_ids: readonly string[];
  readonly package_ids: readonly string[];
  readonly application_refs: readonly string[];
  readonly current_reviewed_pairing: boolean;
}

export interface AppInteractiveResourceCeilings {
  readonly max_sessions: number;
  readonly max_steps: number;
  readonly max_duration_seconds: number;
  readonly max_evidence_bytes: number;
  readonly max_evidence_nodes: number;
  readonly max_pixels: number;
  readonly max_artifact_bytes: number;
  readonly max_output_bytes: number;
}

export interface AppInteractiveExpirySessionPosture {
  readonly grant_lifetime_seconds: number;
  readonly max_session_seconds: number;
  readonly session: AppInteractiveSessionPosture;
}

export interface AppInteractiveCapabilityRequest {
  readonly schema: typeof APP_INTERACTIVE_CAPABILITY_REQUEST_SCHEMA;
  readonly owner: AppInteractiveOwnerKind;
  readonly allowed_origins: readonly string[];
  readonly target_profile_class: AppInteractiveTargetProfileClass;
  readonly target_selectors: AppInteractiveTargetSelectors;
  readonly action_classes: readonly AppInteractiveActionClass[];
  readonly background: AppInteractiveBackgroundPosture;
  readonly capture: AppInteractiveCapturePosture;
  readonly transfer: AppInteractiveTransferPosture;
  readonly resources: AppInteractiveResourceCeilings;
  readonly expiry_session: AppInteractiveExpirySessionPosture;
}

export interface AppInteractiveCapabilityRequestInput {
  readonly owner: AppInteractiveOwnerKind;
  readonly allowedOrigins?: readonly string[];
  readonly targetProfileClass: AppInteractiveTargetProfileClass;
  readonly bundleIds?: readonly string[];
  readonly packageIds?: readonly string[];
  readonly applicationRefs?: readonly string[];
  readonly currentReviewedPairing?: boolean;
  readonly actionClasses: readonly AppInteractiveActionClass[];
  readonly background: AppInteractiveBackgroundPosture;
  readonly capture: AppInteractiveCapturePosture;
  readonly transfer: AppInteractiveTransferPosture;
  readonly resources: AppInteractiveResourceCeilings;
  readonly expirySession: AppInteractiveExpirySessionPosture;
}

const MAX_SAFE = Number.MAX_SAFE_INTEGER;
const REF = /^[A-Za-z][A-Za-z0-9_.-]*:[A-Za-z0-9][A-Za-z0-9:._/-]*$/;
const BUNDLE_OR_PACKAGE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,254}$/;

function integer(value: number, label: string, minimum: number, maximum: number): number {
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum || value > MAX_SAFE) {
    throw new TypeError(`${label} is outside its reviewed integer ceiling`);
  }
  return value;
}

function sortedUnique(values: readonly string[], label: string, pattern?: RegExp): readonly string[] {
  const sorted = [...values].sort((left, right) => left < right ? -1 : left > right ? 1 : 0);
  if (sorted.length > 64 || sorted.some((value, index) =>
    value.length === 0 || value.length > 512 || (pattern !== undefined && !pattern.test(value))
      || (index > 0 && value === sorted[index - 1]))) {
    throw new TypeError(`${label} must contain bounded unique canonical values`);
  }
  return Object.freeze(sorted);
}

function reviewedOrigin(value: string): boolean {
  if (value === "about:blank") return true;
  try {
    const origin = new URL(value);
    return origin.protocol === "https:" && origin.username === "" && origin.password === ""
      && origin.pathname === "/" && origin.search === "" && origin.hash === ""
      && origin.origin === value;
  } catch {
    return false;
  }
}

/** Deterministic authoring builder for one exact reviewed physical-owner action class. */
export function defineInteractiveCapabilityRequest(
  input: AppInteractiveCapabilityRequestInput,
): Readonly<AppInteractiveCapabilityRequest> {
  const allowedOrigins = sortedUnique(input.allowedOrigins ?? [], "allowed origins");
  if (allowedOrigins.some((origin) => !reviewedOrigin(origin))) {
    throw new TypeError("allowed origins must be exact HTTPS origins or about:blank");
  }
  const actionClasses = sortedUnique(input.actionClasses, "action classes");
  const actionClass = actionClasses[0];
  if (actionClasses.length !== 1 || actionClass === undefined
      || input.background !== "direct_owner"
      || input.transfer !== "denied"
      || !["invocation_bound", "run_bound"].includes(input.expirySession.session)) {
    throw new TypeError("interactive requests must select one exact direct-owner action class");
  }
  const bundleIds = sortedUnique(input.bundleIds ?? [], "bundle ids", BUNDLE_OR_PACKAGE);
  const packageIds = sortedUnique(input.packageIds ?? [], "package ids", BUNDLE_OR_PACKAGE);
  const applicationRefs = sortedUnique(input.applicationRefs ?? [], "application refs", REF);
  const currentReviewedPairing = input.currentReviewedPairing ?? false;
  const browser = input.owner === "browser"
    && input.targetProfileClass === "installation_ephemeral_headless"
    && allowedOrigins.length > 0 && bundleIds.length === 0 && packageIds.length === 0
    && applicationRefs.length === 0 && !currentReviewedPairing
    && ["observe", "navigate_or_launch", "interact", "outward_commit"].includes(actionClass)
    && input.capture === "structured_evidence_only";
  const macos = input.owner === "macos"
    && input.targetProfileClass === "owner_reviewed_macos_pairing"
    && allowedOrigins.length === 0 && packageIds.length === 0
    && (bundleIds.length > 0 || applicationRefs.length > 0 || currentReviewedPairing)
    && ["observe", "navigate_or_launch", "interact", "outward_commit"].includes(actionClass)
    && input.capture === "structured_evidence_only";
  const android = input.owner === "android"
    && input.targetProfileClass === "owner_reviewed_android_pairing"
    && allowedOrigins.length === 0 && bundleIds.length === 0
    && (packageIds.length > 0 || applicationRefs.length > 0 || currentReviewedPairing)
    && ["observe", "navigate_or_launch", "interact", "capture_pixels", "outward_commit"].includes(actionClass)
    && input.capture === (actionClass === "capture_pixels" ? "reviewed_pixels" : "structured_evidence_only");
  if (!browser && !macos && !android) throw new TypeError("interactive owner target selectors are inconsistent");
  const resources = Object.freeze({
    max_sessions: integer(input.resources.max_sessions, "max sessions", 1, 32),
    max_steps: integer(input.resources.max_steps, "max steps", 1, 10_000),
    max_duration_seconds: integer(input.resources.max_duration_seconds, "max duration", 1, 86_400),
    max_evidence_bytes: integer(input.resources.max_evidence_bytes, "max evidence bytes", 1, 16 * 1024 * 1024),
    max_evidence_nodes: integer(input.resources.max_evidence_nodes, "max evidence nodes", 0, 64 * 1024),
    max_pixels: integer(input.resources.max_pixels, "max pixels", 0, 16_777_216),
    max_artifact_bytes: integer(input.resources.max_artifact_bytes, "max artifact bytes", 0, 64 * 1024 * 1024),
    max_output_bytes: integer(input.resources.max_output_bytes, "max output bytes", 1, 16 * 1024 * 1024),
  });
  const expirySession = Object.freeze({
    grant_lifetime_seconds: integer(input.expirySession.grant_lifetime_seconds, "grant lifetime", 1, 30 * 86_400),
    max_session_seconds: integer(input.expirySession.max_session_seconds, "session lifetime", 1, 86_400),
    session: input.expirySession.session,
  });
  if (expirySession.max_session_seconds > expirySession.grant_lifetime_seconds) {
    throw new TypeError("session lifetime exceeds grant lifetime");
  }
  return Object.freeze({
    schema: APP_INTERACTIVE_CAPABILITY_REQUEST_SCHEMA,
    owner: input.owner,
    allowed_origins: allowedOrigins,
    target_profile_class: input.targetProfileClass,
    target_selectors: Object.freeze({
      bundle_ids: bundleIds,
      package_ids: packageIds,
      application_refs: applicationRefs,
      current_reviewed_pairing: currentReviewedPairing,
    }),
    action_classes: actionClasses as readonly AppInteractiveActionClass[],
    background: input.background,
    capture: input.capture,
    transfer: input.transfer,
    resources,
    expiry_session: expirySession,
  });
}

export interface AppInteractiveGrantSelection {
  readonly dependency_ref: string;
  readonly reviewed_request_digest: string;
  readonly granted: AppInteractiveCapabilityRequest;
}

export function defineInteractiveGrantSelection(
  dependencyRef: string,
  reviewedRequestDigest: string,
  granted: AppInteractiveCapabilityRequestInput,
): Readonly<AppInteractiveGrantSelection> {
  if (!REF.test(dependencyRef) || !/^blake3:[0-9a-f]{64}$/.test(reviewedRequestDigest)) {
    throw new TypeError("interactive grant selection identity is invalid");
  }
  return Object.freeze({
    dependency_ref: dependencyRef,
    reviewed_request_digest: reviewedRequestDigest,
    granted: defineInteractiveCapabilityRequest(granted),
  });
}

declare const interactiveOpaque: unique symbol;
type OpaqueInteractive<Value extends string, Kind extends string> =
  Value & { readonly [interactiveOpaque]: Kind };
export type AppInteractiveSessionRef = OpaqueInteractive<string, "session">;
export type AppInteractiveObservationRef = OpaqueInteractive<string, "observation">;
export type AppInteractiveReceiptRef = OpaqueInteractive<string, "receipt">;
export type AppInteractiveStopRef = OpaqueInteractive<string, "stop">;
export type AppInteractiveStatusRef = OpaqueInteractive<string, "status">;
"#.to_owned()
}

fn render_typescript_sdk_contract(
    protocol_version: &str,
    operations: &[magician_app_contract::AppPublicOperation],
) -> Result<String> {
    let operation_ids = operations
        .iter()
        .map(|operation| format!("  | \"{}\"", operation.operation_id))
        .collect::<Vec<_>>()
        .join("\n");
    let operations_json = serde_json::to_string_pretty(operations)
        .context("serializing TypeScript SDK public operation inventory")?;
    let deprecations_json = serde_json::to_string_pretty(
        &AppContractCapabilities::current(contract_capability_limits()).deprecations,
    )
    .context("serializing TypeScript SDK public deprecation inventory")?;
    let manifest_schema_versions = serde_json::to_string(&[APP_MANIFEST_SCHEMA_VERSION])
        .context("serializing TypeScript SDK manifest schema versions")?;
    let manifest_features = serde_json::to_string_pretty(AppManifestFeature::supported())
        .context("serializing TypeScript SDK manifest features")?;
    Ok(format!(
        "// Generated by `make app-contract-codegen`; do not edit independently.\n\
import type {{ AppPublicOperation }} from \"../public-contract.js\";\n\n\
export const APP_SUPPORTED_PUBLIC_CONTRACT_VERSION = \"{APP_SUPPORTED_PUBLIC_CONTRACT_VERSION}\" as const;\n\
export const APP_DATA_PLANE_PROTOCOL_VERSION = \"{protocol_version}\" as const;\n\
export const APP_CONTRACT_CAPABILITIES_SCHEMA_VERSION = 1 as const;\n\
export const APP_JSON_SCHEMA_DIALECT = \"{APP_JSON_SCHEMA_DIALECT}\" as const;\n\
export const APP_SUPPORTED_PUBLIC_API_PREFIX = \"/api/magician/v2/apps\" as const;\n\
export const APP_SUPPORTED_MANIFEST_SCHEMA_VERSIONS = {manifest_schema_versions} as const;\n\
export const APP_SUPPORTED_MANIFEST_FEATURES = {manifest_features} as const;\n\n\
export const APP_SUPPORTED_PUBLIC_DEPRECATIONS = {deprecations_json} as const;\n\n\
/** Runtime validators require every JSON integer to be exactly representable in JavaScript. */\n\
export type AppJsonSafeInteger = number;\n\n\
export type AppPublicOperationId =\n{operation_ids};\n\n\
export const APP_SUPPORTED_PUBLIC_OPERATIONS = {operations_json} as const satisfies readonly AppPublicOperation[];\n\n\
export type SupportedPublicErrorReason =\n\
  (typeof APP_SUPPORTED_PUBLIC_OPERATIONS)[number][\"errors\"][number];\n\
/** @deprecated Use SupportedPublicErrorReason; public HTTP errors now use AppErrorEnvelope. */\n\
export type SupportedPublicHttpErrorCode = SupportedPublicErrorReason;\n"
    ))
}

fn contract_fixtures() -> Result<BTreeMap<String, Value>> {
    let mut fixtures = canonical_app_contract_fixtures()
        .context("constructing canonical app data-plane fixtures")?
        .into_iter()
        .map(|fixture| (fixture.name.to_owned(), fixture.value))
        .collect::<BTreeMap<_, _>>();
    insert_public_action_fixtures(&mut fixtures)?;
    let capabilities = AppContractCapabilities::current(contract_capability_limits());
    fixtures.insert(
        "contract_capabilities".to_owned(),
        serde_json::to_value(&capabilities)
            .context("serializing app contract-capabilities fixture")?,
    );
    Ok(fixtures)
}

fn insert_public_action_fixtures(fixtures: &mut BTreeMap<String, Value>) -> Result<()> {
    let mut result: AppActionResult<Value> = serde_json::from_value(
        fixtures
            .get("action_result_completed")
            .cloned()
            .context("completed action-result fixture must exist")?,
    )
    .context("decoding completed action-result fixture")?;
    let run_ref = AppReference::parse("run:app-action:fixture_completed")?;
    result.run_ref = run_ref.clone();
    let run_handle = AppRunHandle {
        protocol_version: AppProtocolVersion::V1,
        run_ref,
        installation_id: AppInstallationId::parse("install_contract_fixture")?,
        action_id: result.action_id.clone(),
    };
    let launch = AppActionLaunchResponse::<Value> {
        run_handle: run_handle.clone(),
        execution_id: Some("execution_fixture_1".to_owned()),
        result: None,
    };
    let running = AppRunSnapshot::<Value> {
        protocol_version: AppProtocolVersion::V1,
        run_handle: run_handle.clone(),
        execution_id: Some("execution_fixture_1".to_owned()),
        status: AppRunStatus::Running,
        terminal: false,
        cancellation_generation: None,
        result_withheld: false,
        result: None,
    };
    let completed = AppRunSnapshot {
        protocol_version: AppProtocolVersion::V1,
        run_handle,
        execution_id: Some("execution_fixture_1".to_owned()),
        status: AppRunStatus::Completed,
        terminal: true,
        cancellation_generation: None,
        result_withheld: false,
        result: Some(result),
    };
    let limits = magician::magician_v2::apps::models::AppContractLimits::default();
    running.validate_app_contract(&limits)?;
    completed.validate_app_contract(&limits)?;
    fixtures.insert(
        "action_launch".to_owned(),
        serde_json::to_value(launch).context("serializing public action-launch fixture")?,
    );
    fixtures.insert(
        "action_run_running".to_owned(),
        serde_json::to_value(running).context("serializing running action-run fixture")?,
    );
    fixtures.insert(
        "action_run_completed".to_owned(),
        serde_json::to_value(completed).context("serializing completed action-run fixture")?,
    );
    fixtures.insert(
        "action_cancellation_request".to_owned(),
        json!({
            "expected_generation": 0,
            "idempotency_key": "cancel:fixture:1"
        }),
    );
    fixtures.insert(
        "action_cancellation_receipt".to_owned(),
        json!({
            "protocol_version": "1",
            "run_ref": "run:app-action:fixture_running",
            "generation": 1,
            "idempotency_key": "cancel:fixture:1",
            "status": "cancelling",
            "requested_at": "2026-01-01T00:00:00Z"
        }),
    );
    fixtures.insert(
        "action_composition_request".to_owned(),
        serde_json::to_value(AppActionCompositionRequest {
            destination_installation_id: AppInstallationId::parse("install_contract_destination")?,
            destination_action_id: magician::magician_v2::apps::models::AppName::parse(
                "accept_plan",
            )?,
            mapping: vec![AppValueMappingOperation::Select {
                source: magician::magician_v2::apps::models::AppFieldPath::parse("plan")?,
                target: magician::magician_v2::apps::models::AppFieldPath::parse("plan")?,
            }],
            idempotency_key: AppReference::parse("compose:fixture:1")?,
            chain: Vec::new(),
            subscription: None,
        })
        .context("serializing action-composition request fixture")?,
    );
    fixtures.insert(
        "action_composition_waiting".to_owned(),
        serde_json::to_value(AppActionCompositionResponse {
            result: AppActionResultComposition::Waiting {
                source_run: AppRunHandle {
                    protocol_version: AppProtocolVersion::V1,
                    run_ref: AppReference::parse("run:app-action:fixture_source")?,
                    installation_id: AppInstallationId::parse("install_contract_source")?,
                    action_id: magician::magician_v2::apps::models::AppName::parse("build_plan")?,
                },
            },
            chain: AppActionCompositionChainProgress {
                origin_source_run_ref: AppReference::parse("run:app-action:fixture_source")?,
                active_source_run_ref: AppReference::parse("run:app-action:fixture_source")?,
                active_destination_installation_id: AppInstallationId::parse(
                    "install_contract_destination",
                )?,
                active_destination_action_id: magician::magician_v2::apps::models::AppName::parse(
                    "accept_plan",
                )?,
                hop_index: 0,
                hop_count: 1,
            },
            subscription: None,
        })
        .context("serializing action-composition response fixture")?,
    );
    Ok(())
}

fn contract_schemas() -> Result<BTreeMap<String, Value>> {
    let mut schemas = BTreeMap::new();
    insert_schema::<AppDataEnvelope<Value>>(&mut schemas, "app-data-envelope")?;
    insert_schema::<AppQueryRequest>(&mut schemas, "app-query-request")?;
    insert_schema::<AppQueryPage>(&mut schemas, "app-query-page")?;
    insert_schema::<AppMutationCommand>(&mut schemas, "app-mutation-command")?;
    insert_schema::<AppActionInvocation<Value>>(&mut schemas, "app-action-invocation")?;
    insert_schema::<AppActionResult<Value>>(&mut schemas, "app-action-result")?;
    insert_schema::<AppDataEnvelope<AppArtifactProjection>>(
        &mut schemas,
        "app-artifact-projection",
    )?;
    insert_schema::<AppErrorEnvelope>(&mut schemas, "app-error-envelope")?;
    insert_schema::<AppContractCapabilities>(&mut schemas, "app-contract-capabilities")?;
    insert_schema::<AppDirectActionRequest>(&mut schemas, "app-direct-action-request")?;
    insert_schema::<AppMutationReceipt>(&mut schemas, "app-mutation-receipt")?;
    insert_schema::<AppActionLaunchResponse<Value>>(&mut schemas, "app-action-launch-response")?;
    insert_schema::<AppRunSnapshot<Value>>(&mut schemas, "app-run-snapshot")?;
    insert_schema::<AppActionCancellationRequest>(&mut schemas, "app-action-cancellation-request")?;
    insert_schema::<AppActionCancellationReceipt>(&mut schemas, "app-action-cancellation-receipt")?;
    insert_schema::<AppActionCompositionRequest>(&mut schemas, "app-action-composition-request")?;
    insert_schema::<AppActionCompositionResponse>(&mut schemas, "app-action-result-composition")?;
    insert_schema::<AppEntityChangeBatch>(&mut schemas, "app-entity-change-batch")?;
    Ok(schemas)
}

fn contract_capability_limits() -> AppContractCapabilityLimits {
    let limits = magician::magician_v2::apps::models::AppContractLimits::default();
    AppContractCapabilityLimits {
        max_document_bytes: limits.max_document_bytes() as u64,
        max_json_depth: limits.max_json_depth() as u64,
        max_json_nodes: limits.max_json_nodes() as u64,
        max_value_bytes: limits.max_value_bytes() as u64,
        max_value_nodes: limits.max_value_nodes() as u64,
        max_collection_items: limits.max_collection_items() as u64,
        max_predicate_nodes: limits.max_predicate_nodes() as u64,
        max_predicate_depth: limits.max_predicate_depth() as u64,
        max_page_rows: limits.max_page_rows() as u64,
        max_entity_change_page_rows: MAX_APP_ENTITY_CHANGE_LIMIT as u64,
    }
}

fn insert_schema<T>(schemas: &mut BTreeMap<String, Value>, name: &str) -> Result<()>
where
    T: JsonSchema,
{
    let mut schema = schema_to_value(schema_for!(T))?;
    let object = schema
        .as_object_mut()
        .context("generated JSON Schema root must be an object")?;
    object.insert(
        "$id".to_owned(),
        Value::String(format!(
            "https://magican.ai/contracts/apps/v1/{name}.schema.json"
        )),
    );
    schemas.insert(name.to_owned(), schema);
    Ok(())
}

fn schema_to_value(schema: Schema) -> Result<Value> {
    serde_json::to_value(schema).context("serializing generated JSON Schema")
}

fn digest_named_values(values: &BTreeMap<String, Value>) -> Result<String> {
    let mut canonical = Vec::new();
    for (name, value) in values {
        canonical.extend_from_slice(name.as_bytes());
        canonical.push(0);
        canonical.extend_from_slice(
            &serde_json::to_vec(value).context("serializing contract digest input")?,
        );
        canonical.push(0xff);
    }
    Ok(format!("blake3:{}", blake3::hash(&canonical).to_hex()))
}

/// Build the supported-public OpenAPI document from the same immutable
/// operation descriptors consumed by API route registration.
fn build_supported_public_openapi(
    schemas: &BTreeMap<String, Value>,
    schema_digest: &str,
    operation_inventory_digest: &str,
) -> Result<Value> {
    let mut components = Map::new();
    for (schema_name, standalone) in schemas {
        let root_name = pascal_case(schema_name);
        let mut root = standalone.clone();
        let root_object = root
            .as_object_mut()
            .context("generated JSON Schema root must be an object")?;
        root_object.remove("$schema");
        root_object.remove("$id");
        let definitions = root_object
            .remove("$defs")
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();

        let definition_names = definitions
            .keys()
            .map(|name| {
                (
                    name.clone(),
                    format!("{root_name}__{}", component_safe(name)),
                )
            })
            .collect::<BTreeMap<_, _>>();
        rewrite_definition_refs(&mut root, &definition_names);
        components.insert(root_name, root);

        for (definition_name, mut definition) in definitions {
            rewrite_definition_refs(&mut definition, &definition_names);
            let component_name = definition_names
                .get(&definition_name)
                .context("definition component mapping must exist")?;
            components.insert(component_name.clone(), definition);
        }
    }

    let mut paths = Map::new();
    for operation in public_operation_inventory() {
        let mut parameters = Vec::new();
        for parameter in &operation.parameters {
            let location = match parameter.location {
                AppOperationParameterLocation::Path => "path",
                AppOperationParameterLocation::Query => "query",
            };
            let mut schema = serde_json::Map::new();
            schema.insert(
                "type".to_owned(),
                Value::String(parameter.schema_type.to_owned()),
            );
            if let Some(format) = &parameter.schema_format {
                schema.insert("format".to_owned(), Value::String(format.to_owned()));
            }
            if let Some(minimum) = parameter.minimum {
                schema.insert("minimum".to_owned(), Value::from(minimum));
            }
            if let Some(maximum) = parameter.maximum {
                schema.insert("maximum".to_owned(), Value::from(maximum));
            }
            if let Some(default) = parameter.default {
                schema.insert("default".to_owned(), Value::from(default));
            }
            parameters.push(json!({
                "name": parameter.name,
                "in": location,
                "required": parameter.required,
                "description": parameter.description,
                "schema": Value::Object(schema)
            }));
        }
        let mut responses = Map::new();
        for status in &operation.success_statuses {
            responses.insert(
                status.to_string(),
                json!({
                    "description": "Successful admitted operation.",
                    "content": {
                        "application/json": {
                            "schema": {
                                "$ref": format!(
                                    "#/components/schemas/{}",
                                    operation.response_schema
                                )
                            }
                        }
                    }
                }),
            );
        }
        responses.insert(
            "default".to_owned(),
            json!({
                "description": "Canonical supported-public error. Retry and uncertainty behavior is determined only by disposition, never by status or display text.",
                "content": {
                    "application/json": {
                        "schema": { "$ref": "#/components/schemas/AppErrorEnvelope" }
                    }
                },
                "x-magician-error-reasons": operation.errors
            }),
        );
        let mut document = json!({
            "operationId": operation.operation_id,
            "summary": operation.summary,
            "parameters": parameters,
            "responses": responses,
            "x-magician-auth-posture": operation.auth,
            "x-magician-idempotency": operation.idempotency,
            "x-magician-auth-established-out-of-band": true
        });
        if let Some(request_schema) = operation.request_schema {
            document
                .as_object_mut()
                .expect("operation document is an object")
                .insert(
                    "requestBody".to_owned(),
                    json!({
                        "required": true,
                        "content": {
                            "application/json": {
                                "schema": {
                                    "$ref": format!("#/components/schemas/{request_schema}")
                                }
                            }
                        }
                    }),
                );
        }
        if let Some(example) = operation.request_example {
            document
                .as_object_mut()
                .expect("operation document is an object")
                .insert("x-magician-request-fixture".to_owned(), json!(example));
        }
        if let Some(example) = operation.response_example {
            document
                .as_object_mut()
                .expect("operation document is an object")
                .insert("x-magician-response-fixture".to_owned(), json!(example));
        }
        let method = match operation.method {
            AppHttpMethod::Get => "get",
            AppHttpMethod::Post => "post",
        };
        let path = format!("/api/magician/v2/apps{}", operation.path);
        paths
            .entry(path)
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("OpenAPI path item is an object")
            .insert(method.to_owned(), document);
    }

    Ok(json!({
        "openapi": "3.1.0",
        "jsonSchemaDialect": APP_JSON_SCHEMA_DIALECT,
        "info": {
            "title": "Magician App Data Plane",
            "version": APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
            "description": "Supported-public Apps operations only. Authentication is established by the Magician host and is not a portable credential contract."
        },
        "paths": paths,
        "components": { "schemas": components },
        "x-magician-protocol-version": AppProtocolVersion::V1.as_str(),
        "x-magician-schema-digest": schema_digest,
        "x-magician-operation-inventory-digest": operation_inventory_digest,
        "x-magician-runtime-routes-enabled": true
    }))
}

fn rewrite_definition_refs(value: &mut Value, mappings: &BTreeMap<String, String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                rewrite_definition_refs(value, mappings);
            }
        },
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.get_mut("$ref") {
                if let Some(definition) = reference.strip_prefix("#/$defs/") {
                    if let Some(component) = mappings.get(definition) {
                        *reference = format!("#/components/schemas/{component}");
                    }
                }
            }
            for value in object.values_mut() {
                rewrite_definition_refs(value, mappings);
            }
        },
        _ => {},
    }
}

fn render_typescript_fixture_contract(
    protocol_version: &str,
    schema_digest: &str,
    fixture_digest: &str,
    fixtures: &BTreeMap<String, Value>,
) -> Result<String> {
    let fixture_json = serde_json::to_string_pretty(fixtures)
        .context("serializing TypeScript contract fixtures")?;
    Ok(format!(
        "// Generated by `make app-contract-codegen`; do not edit.\n\
     export const APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION = {component_contract_version:?} as const;\n\
     export const APP_SUPPORTED_PUBLIC_CONTRACT_VERSION = {public_contract_version:?} as const;\n\
     /** @deprecated Use the explicitly named contract-axis version. */\n\
     export const APP_CONTRACT_VERSION = APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION;\n\
         export const APP_PROTOCOL_VERSION = {protocol_version:?} as const;\n\
         export const APP_SCHEMA_DIGEST = {schema_digest:?} as const;\n\
         export const APP_FIXTURE_DIGEST = {fixture_digest:?} as const;\n\n\
         export const APP_CONTRACT_FIXTURES = {fixture_json} as const;\n\n\
         export type AppContractFixtureName = keyof typeof APP_CONTRACT_FIXTURES;\n",
        component_contract_version = APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
        public_contract_version = APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
    ))
}

fn render_swift_fixture_contract(
    protocol_version: &str,
    schema_digest: &str,
    fixture_digest: &str,
    fixtures: &BTreeMap<String, Value>,
) -> Result<String> {
    let mut rendered = String::from(
        "// Generated by `make app-contract-codegen`; do not edit.\n\
         import Foundation\n\n\
         enum AppContractFixturesGenerated {\n",
    );
    rendered.push_str(&format!(
        "    static let dataPlaneComponentContractVersion = {component_contract_version:?}\n\
     static let supportedPublicContractVersion = {public_contract_version:?}\n\
     static let contractVersion = dataPlaneComponentContractVersion\n\
         static let protocolVersion = {protocol:?}\n\
         static let schemaDigest = {schema:?}\n\
         static let fixtureDigest = {fixture:?}\n\n\
         static let jsonByName: [String: String] = [\n",
        component_contract_version = APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION,
        public_contract_version = APP_SUPPORTED_PUBLIC_CONTRACT_VERSION,
        protocol = protocol_version,
        schema = schema_digest,
        fixture = fixture_digest,
    ));
    for (name, value) in fixtures {
        let json =
            serde_json::to_string_pretty(value).context("serializing Swift contract fixture")?;
        if json.contains("\"\"\"#") {
            bail!("fixture `{name}` cannot be represented by the generated Swift raw string");
        }
        rendered.push_str(&format!("        {name:?}: #\"\"\"\n{json}\n\"\"\"#,\n"));
    }
    rendered.push_str("    ]\n}\n");
    Ok(rendered)
}

fn pascal_case(value: &str) -> String {
    value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut characters = part.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect(),
                None => String::new(),
            }
        })
        .collect()
}

fn component_safe(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn json_artifact(path: impl Into<PathBuf>, value: &Value) -> Result<Artifact> {
    let mut contents = serde_json::to_vec_pretty(value).context("rendering JSON artifact")?;
    contents.push(b'\n');
    Ok(Artifact {
        relative_path: path.into(),
        contents,
    })
}

fn text_artifact(path: impl Into<PathBuf>, contents: String) -> Artifact {
    Artifact {
        relative_path: path.into(),
        contents: contents.into_bytes(),
    }
}

fn write_artifacts(repo_root: &Path, artifacts: &[Artifact]) -> Result<()> {
    for artifact in artifacts {
        let path = repo_root.join(&artifact.relative_path);
        if path.exists() && fs::read(&path)? == artifact.contents {
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, &artifact.contents)
            .with_context(|| format!("writing {}", path.display()))?;
        println!("generated {}", artifact.relative_path.display());
    }
    Ok(())
}

fn check_artifacts(repo_root: &Path, artifacts: &[Artifact]) -> Result<()> {
    let stale = artifacts
        .iter()
        .filter_map(|artifact| {
            let path = repo_root.join(&artifact.relative_path);
            match fs::read(&path) {
                Ok(contents) if contents == artifact.contents => None,
                Ok(_) => Some(format!("stale {}", artifact.relative_path.display())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Some(format!("missing {}", artifact.relative_path.display()))
                },
                Err(error) => Some(format!(
                    "unreadable {}: {error}",
                    artifact.relative_path.display()
                )),
            }
        })
        .collect::<Vec<_>>();
    if !stale.is_empty() {
        bail!(
            "app contract artifacts are not current:\n{}\nrun `make app-contract-codegen`",
            stale.join("\n")
        );
    }
    println!("app contract artifacts are current");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_public_openapi_has_exact_operation_and_success_status_parity() {
        let schemas = contract_schemas().unwrap();
        let fixtures = contract_fixtures().unwrap();
        let openapi =
            build_supported_public_openapi(&schemas, "blake3:placeholder", "blake3:operations")
                .unwrap();
        let paths = openapi["paths"].as_object().unwrap();
        assert_eq!(paths.len(), public_operation_inventory().len());
        for operation in public_operation_inventory() {
            let path = format!("/api/magician/v2/apps{}", operation.path);
            let method = operation.method.as_openapi_key();
            let document = &paths[&path][method];
            assert_eq!(
                document["operationId"].as_str(),
                Some(operation.operation_id.as_str())
            );
            for status in operation.success_statuses {
                let status = status.to_string();
                assert!(document["responses"].get(status.as_str()).is_some());
            }
            if let Some(example) = operation.request_example {
                assert!(fixtures.contains_key(example.as_str()), "missing {example}");
            }
            if let Some(example) = operation.response_example {
                assert!(fixtures.contains_key(example.as_str()), "missing {example}");
            }
        }
        assert!(paths.keys().all(|path| !path.contains("custom-surface")));
        assert!(paths
            .keys()
            .all(|path| !path.contains("/actions/{action_id}/invocations")));
        let launch = &openapi["components"]["schemas"]["AppActionLaunchResponse"];
        assert!(launch["properties"].get("task_id").is_none());
        assert!(launch["properties"].get("run_handle").is_some());
        for operation in public_operation_inventory() {
            let path = format!("/api/magician/v2/apps{}", operation.path);
            let default = &paths[&path][operation.method.as_openapi_key()]["responses"]["default"];
            assert_eq!(
                default["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/AppErrorEnvelope"
            );
            assert!(default.get("x-magician-error-reasons").is_some());
            assert!(default.get("x-magician-error-codes").is_none());
        }
        assert!(openapi["components"]["schemas"]
            .get("AppLegacyHttpError")
            .is_none());

        let parameters = paths
            ["/api/magician/v2/apps/installations/{installation_id}/entity-changes"]["get"]
            ["parameters"]
            .as_array()
            .expect("entity changes parameters");
        let schema = |name: &str| {
            &parameters
                .iter()
                .find(|parameter| parameter["name"] == name)
                .unwrap_or_else(|| panic!("missing {name} parameter"))["schema"]
        };
        assert!(schema("after_change_sequence").get("format").is_none());
        assert_eq!(schema("after_change_sequence")["minimum"].as_u64(), Some(0));
        assert!(schema("surface_revision").get("format").is_none());
        assert_eq!(schema("surface_revision")["minimum"].as_u64(), Some(1));
        assert!(schema("limit").get("format").is_none());
        assert_eq!(schema("limit")["minimum"].as_u64(), Some(1));
        assert_eq!(schema("limit")["maximum"].as_u64(), Some(128));
        assert_eq!(schema("limit")["default"].as_u64(), Some(64));
    }

    #[test]
    fn generated_client_mirrors_name_both_version_axes_explicitly() {
        let artifacts = build_artifacts().expect("build contract artifacts");
        let typescript_artifact = artifacts
            .iter()
            .find(|artifact| artifact.relative_path == PathBuf::from(TYPESCRIPT_PATH))
            .expect("TypeScript fixture artifact");
        let typescript = std::str::from_utf8(&typescript_artifact.contents)
            .expect("TypeScript fixture artifact is UTF-8");
        assert!(typescript.contains(
            "export const APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION = \"1.0.0\" as const;"
        ));
        assert!(typescript.contains(&format!(
            "export const APP_SUPPORTED_PUBLIC_CONTRACT_VERSION = \"{APP_SUPPORTED_PUBLIC_CONTRACT_VERSION}\" as const;"
        )));
        assert!(typescript.contains(
            "export const APP_CONTRACT_VERSION = APP_DATA_PLANE_COMPONENT_CONTRACT_VERSION;"
        ));

        let swift_artifact = artifacts
            .iter()
            .find(|artifact| artifact.relative_path == PathBuf::from(SWIFT_PATH))
            .expect("Swift fixture artifact");
        let swift =
            std::str::from_utf8(&swift_artifact.contents).expect("Swift fixture artifact is UTF-8");
        assert!(swift.contains("static let dataPlaneComponentContractVersion = \"1.0.0\""));
        assert!(swift.contains(&format!(
            "static let supportedPublicContractVersion = \"{APP_SUPPORTED_PUBLIC_CONTRACT_VERSION}\""
        )));
        assert!(swift.contains("static let contractVersion = dataPlaneComponentContractVersion"));

        let sdk_artifact = artifacts
            .iter()
            .find(|artifact| artifact.relative_path == PathBuf::from(TYPESCRIPT_SDK_CONTRACT_PATH))
            .expect("TypeScript supported-public SDK artifact");
        let sdk = std::str::from_utf8(&sdk_artifact.contents)
            .expect("TypeScript supported-public SDK artifact is UTF-8");
        assert!(sdk.contains(&format!(
            "export const APP_SUPPORTED_PUBLIC_CONTRACT_VERSION = \"{APP_SUPPORTED_PUBLIC_CONTRACT_VERSION}\" as const;"
        )));
        assert!(sdk.contains("export type AppPublicOperationId ="));
        assert!(sdk.contains("export type AppJsonSafeInteger = number;"));
        assert!(sdk.contains("export type SupportedPublicErrorReason ="));
        assert!(
            sdk.contains("export type SupportedPublicHttpErrorCode = SupportedPublicErrorReason;")
        );
        assert!(sdk.contains("export const APP_SUPPORTED_PUBLIC_DEPRECATIONS ="));
        assert!(sdk.contains("metadata.magician.app_sdk_version"));
        for operation in public_operation_inventory() {
            assert!(sdk.contains(&format!("  | \"{}\"", operation.operation_id)));
            assert!(sdk.contains(&format!("\"operation_id\": \"{}\"", operation.operation_id)));
            assert!(sdk.contains(&format!("\"path\": \"{}\"", operation.path)));
        }
        for forbidden in ["/launch\"", "custom-surface", "provider", "task_id"] {
            assert!(!sdk.contains(forbidden), "SDK contains `{forbidden}`");
        }
    }

    #[test]
    fn every_generated_openapi_schema_reference_resolves() {
        let schemas = contract_schemas().unwrap();
        let openapi =
            build_supported_public_openapi(&schemas, "blake3:placeholder", "blake3:operations")
                .unwrap();
        let components = openapi["components"]["schemas"].as_object().unwrap();
        let mut pending = vec![&openapi];
        while let Some(value) = pending.pop() {
            match value {
                Value::Array(values) => pending.extend(values),
                Value::Object(object) => {
                    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                        let component = reference
                            .strip_prefix("#/components/schemas/")
                            .expect("OpenAPI refs stay within the generated schema catalog");
                        assert!(components.contains_key(component), "missing {component}");
                    }
                    pending.extend(object.values());
                },
                _ => {},
            }
        }
    }
}
