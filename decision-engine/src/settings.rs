//! The engine owns routing validation and persistence; hosts only authenticate/proxy.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use actix_web::{web, HttpResponse};
use decision_engine_contract::settings::*;
use magician_decision::config::{fingerprint, DecisionConfig};
use magician_decision::engine::route_names_for;
use serde_json::json;

use crate::Engine;

pub(crate) struct SettingsStore {
    path: PathBuf,
    root: PathBuf,
    writes: Mutex<()>,
}
impl SettingsStore {
    pub fn new(path: PathBuf, root: PathBuf) -> Self {
        Self {
            path,
            root,
            writes: Mutex::new(()),
        }
    }

    fn read(&self) -> Result<(String, DecisionConfig), String> {
        let text = std::fs::read_to_string(&self.path).map_err(|e| e.to_string())?;
        let config = crate::parse_config(&text, &self.path)?;
        Ok((text, config))
    }

    fn view(&self, engine: &Engine) -> Result<RoutingSettings, String> {
        let (text, config) = self.read()?;
        let current = engine.current();
        let mut prepared = config.clone();
        crate::resolve_model_dirs(
            &mut prepared,
            &self.root,
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        );
        let active = current.operations();
        let operations = config
            .operations
            .iter()
            .map(|(name, op)| {
                let local = route_names_for(&config, op, true);
                let cloud = route_names_for(&config, op, false);
                let short = |names: &[String]| -> Option<ModelRoute> {
                    if names.is_empty() || names.len() > 2 {
                        return None;
                    }
                    Some(ModelRoute {
                        primary: names[0].clone(),
                        backup: names.get(1).cloned(),
                    })
                };
                let routing = short(&local)
                    .zip(short(&cloud))
                    .map(|(local, cloud)| LocalityRoutes { local, cloud });
                let mut thresholds = op.thresholds_by_model.clone();
                if !op.model.is_empty() {
                    thresholds
                        .entry(op.model.clone())
                        .or_insert_with(|| op.thresholds.clone());
                }
                let bound = active.operations.iter().find(|o| &o.name == name);
                OperationRouting {
                    name: name.clone(),
                    routing,
                    configured_local: local,
                    configured_cloud: cloud,
                    active_local: bound.map(|o| o.route_local.clone()).unwrap_or_default(),
                    active_cloud: bound.map(|o| o.route_cloud.clone()).unwrap_or_default(),
                    allow_remote_when_local: op.allow_remote_when_local,
                    threshold_names: threshold_names(&config, name),
                    thresholds_by_model: thresholds,
                }
            })
            .collect();
        Ok(RoutingSettings {
            contract_version: decision_engine_contract::CONTRACT_VERSION,
            revision: fingerprint(&text),
            pending: prepared != current.config,
            enabled: config.enabled,
            models: config
                .models
                .iter()
                .map(|(name, m)| RoutingModel {
                    name: name.clone(),
                    model: m.model.clone(),
                    remote: m.is_remote(),
                })
                .collect(),
            operations,
        })
    }

    fn update(
        &self,
        engine: &Engine,
        update: RoutingUpdate,
    ) -> Result<RoutingSettings, (u16, String)> {
        let _lock = self.writes.lock().unwrap_or_else(|p| p.into_inner());
        let (text, mut config) = self.read().map_err(|e| (503, e))?;
        if fingerprint(&text) != update.revision {
            return Err((
                409,
                "Settings changed elsewhere. Reload before saving.".into(),
            ));
        }
        validate_update(&config, &update).map_err(|e| (400, e))?;
        let operation = config.operations.get_mut(&update.operation).unwrap();
        operation.routing = Some(update.routing.clone());
        operation.allow_remote_when_local = update.allow_remote_when_local;
        operation.thresholds_by_model = update.thresholds_by_model.clone();
        config.validate().map_err(|e| (400, e))?;
        // Update only owned fields in the original YAML value. Do not serialize
        // normalized model paths, defaults, credentials, or other operations.
        let mut yaml: serde_yaml::Value =
            serde_yaml::from_str(&text).map_err(|e| (400, e.to_string()))?;
        let target = &mut yaml["operations"][&update.operation];
        target["routing"] = serde_yaml::to_value(&update.routing).unwrap();
        target["allow_remote_when_local"] =
            serde_yaml::to_value(update.allow_remote_when_local).unwrap();
        target["thresholds_by_model"] = serde_yaml::to_value(&update.thresholds_by_model).unwrap();
        let next = serde_yaml::to_string(&yaml).map_err(|e| (500, e.to_string()))?;
        let save = || -> std::io::Result<()> {
            let mut temporary = tempfile::NamedTempFile::new_in(self.path.parent().unwrap())?;
            temporary
                .as_file()
                .set_permissions(std::fs::metadata(&self.path)?.permissions())?;
            temporary.write_all(next.as_bytes())?;
            temporary.as_file().sync_all()?;
            // Detect manual edits made while validation was running too.
            if std::fs::read_to_string(&self.path)? != text {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "Settings changed elsewhere. Reload before saving.",
                ));
            }
            temporary.persist(&self.path).map_err(|e| e.error)?;
            Ok(())
        };
        save().map_err(|e| {
            (
                if e.kind() == std::io::ErrorKind::AlreadyExists {
                    409
                } else {
                    500
                },
                e.to_string(),
            )
        })?;
        self.view(engine).map_err(|e| (503, e))
    }
}

fn threshold_names(config: &DecisionConfig, name: &str) -> Vec<String> {
    let op = &config.operations[name];
    op.thresholds
        .keys()
        .chain(op.thresholds_by_model.values().flat_map(|m| m.keys()))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}
fn validate_update(config: &DecisionConfig, update: &RoutingUpdate) -> Result<(), String> {
    let op = config
        .operations
        .get(&update.operation)
        .ok_or("Unknown decision operation")?;
    let heads = threshold_names(config, &update.operation);
    if heads.is_empty() && op.gate.enabled {
        return Err("This operation has no configured threshold schema".into());
    }
    for route in [&update.routing.local, &update.routing.cloud] {
        let mut identities = BTreeMap::new();
        if route.backup.as_ref() == Some(&route.primary) {
            return Err("Primary and backup must differ".into());
        }
        for name in route.names() {
            if !config.models.contains_key(&name) {
                return Err(format!("Unknown model: {name}"));
            }
            let thresholds = update
                .thresholds_by_model
                .get(&name)
                .ok_or_else(|| format!("Set model-specific thresholds for {name}"))?;
            if heads.iter().any(|h| !thresholds.contains_key(h)) {
                return Err(format!("Missing model-specific thresholds for {name}"));
            }
            let model = &config.models[&name];
            if identities
                .insert((&model.adapter, &model.model), thresholds)
                .is_some_and(|previous| previous != thresholds)
            {
                return Err("Profiles sharing an adapter and model identity must use the same thresholds within one route.".into());
            }
        }
    }
    for (name, thresholds) in &update.thresholds_by_model {
        if !config.models.contains_key(name) {
            return Err(format!("Unknown threshold model: {name}"));
        }
        if thresholds
            .values()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
        {
            return Err("Thresholds must be numbers from 0 to 1".into());
        }
    }
    if !update.allow_remote_when_local
        && update
            .routing
            .local
            .names()
            .iter()
            .any(|n| config.models[n].is_remote())
    {
        return Err("Local route includes a remote model. Enable remote use explicitly or select local models.".into());
    }
    Ok(())
}

pub(crate) async fn get(engine: web::Data<Engine>) -> HttpResponse {
    match tokio::task::spawn_blocking(move || {
        engine
            .settings
            .as_ref()
            .ok_or_else(|| "Settings are unavailable".to_string())?
            .view(&engine)
    })
    .await
    {
        Ok(Ok(view)) => HttpResponse::Ok().json(view),
        _ => HttpResponse::ServiceUnavailable()
            .json(json!({"message":"Decision Engine settings are unavailable."})),
    }
}
pub(crate) async fn put(engine: web::Data<Engine>, body: web::Json<RoutingUpdate>) -> HttpResponse {
    match tokio::task::spawn_blocking(move || {
        engine
            .settings
            .as_ref()
            .ok_or((503, "Settings are unavailable".to_string()))?
            .update(&engine, body.into_inner())
    })
    .await
    {
        Ok(Ok(view)) => HttpResponse::Ok().json(view),
        Ok(Err((status, message))) => {
            HttpResponse::build(actix_web::http::StatusCode::from_u16(status).unwrap())
                .json(json!({"message":message}))
        },
        Err(_) => HttpResponse::InternalServerError()
            .json(json!({"message":"Could not save decision routing."})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decision_engine_contract::settings::{LocalityRoutes, ModelRoute};
    const YAML: &str = "enabled: true\nmodels:\n  kev: {adapter: systemone, model: kev, endpoint: 'http://localhost:9999/v1/systemone'}\n  jev: {adapter: systemone, model: jev, endpoint: 'https://example.com/v1/systemone'}\noperations:\n  memory_applicability:\n    model: jev\n    pack: memory_applicability\n    sees_body: true\n    gate: {enabled: true}\n    thresholds: {apply: 0.7}\n    thresholds_by_model:\n      kev: {apply: 0.75}\n";
    fn route(primary: &str, backup: Option<&str>) -> ModelRoute {
        ModelRoute {
            primary: primary.into(),
            backup: backup.map(str::to_owned),
        }
    }
    fn fixture() -> (tempfile::TempDir, Engine) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("decision-engine.yaml");
        std::fs::write(&path, YAML).unwrap();
        let engine = Engine::from_config(crate::load_config(&path).unwrap())
            .with_settings_path(path, dir.path().to_path_buf());
        (dir, engine)
    }
    fn update(view: &RoutingSettings) -> RoutingUpdate {
        RoutingUpdate {
            revision: view.revision.clone(),
            operation: "memory_applicability".into(),
            routing: LocalityRoutes {
                local: route("kev", None),
                cloud: route("jev", Some("kev")),
            },
            allow_remote_when_local: false,
            thresholds_by_model: view.operations[0].thresholds_by_model.clone(),
        }
    }
    #[test]
    fn settings_save_is_revision_guarded_persistent_and_pending_until_bound() {
        let (_dir, engine) = fixture();
        let store = engine.settings.as_ref().unwrap();
        let before = store.view(&engine).unwrap();
        assert!(!before.pending);
        let change = update(&before);
        let saved = store.update(&engine, change.clone()).unwrap();
        assert!(saved.pending);
        assert_eq!(saved.operations[0].configured_local, ["kev"]);
        assert_eq!(saved.operations[0].active_cloud, ["jev"]);
        assert_eq!(store.update(&engine, change).unwrap_err().0, 409);
        let persisted = crate::load_config(&store.path).unwrap();
        assert_eq!(
            persisted.models,
            crate::parse_config(YAML, &store.path).unwrap().models
        );
        assert!(persisted.operations["memory_applicability"].gate.enabled);
        engine.reload(persisted);
        let active = store.view(&engine).unwrap();
        assert!(!active.pending);
        assert_eq!(active.operations[0].active_local, ["kev"]);
        assert_eq!(active.operations[0].active_cloud, ["jev", "kev"]);
        // The opposite direction uses exactly the same primary/backup schema.
        let mut reverse = update(&active);
        reverse.routing.cloud = route("kev", Some("jev"));
        store.update(&engine, reverse).unwrap();
        engine.reload(crate::load_config(&store.path).unwrap());
        assert_eq!(
            store.view(&engine).unwrap().operations[0].active_cloud,
            ["kev", "jev"]
        );
        // Both single-model choices have no implicit backup.
        for model in ["kev", "jev"] {
            let mut single = update(&store.view(&engine).unwrap());
            single.routing.cloud = route(model, None);
            store.update(&engine, single).unwrap();
            engine.reload(crate::load_config(&store.path).unwrap());
            assert_eq!(
                store.view(&engine).unwrap().operations[0].active_cloud,
                [model]
            );
        }
    }
    #[test]
    fn settings_reject_unknown_duplicate_unthresholded_and_unapproved_remote_routes() {
        let (_dir, engine) = fixture();
        let store = engine.settings.as_ref().unwrap();
        let before = store.view(&engine).unwrap();
        let valid = update(&before);
        let mut cases = Vec::new();
        let mut c = valid.clone();
        c.routing.cloud.primary = "missing".into();
        cases.push(c);
        let mut c = valid.clone();
        c.routing.cloud.backup = Some("jev".into());
        cases.push(c);
        let mut c = valid.clone();
        c.thresholds_by_model.remove("kev");
        cases.push(c);
        let mut c = valid.clone();
        c.thresholds_by_model
            .get_mut("kev")
            .unwrap()
            .insert("apply".into(), 1.1);
        cases.push(c);
        let mut c = valid.clone();
        c.routing.local.backup = Some("jev".into());
        cases.push(c);
        for c in cases {
            assert_eq!(store.update(&engine, c).unwrap_err().0, 400);
        }
        assert_eq!(store.view(&engine).unwrap(), before);
        let mut allowed = valid;
        allowed.routing.local.backup = Some("jev".into());
        allowed.allow_remote_when_local = true;
        store.update(&engine, allowed).unwrap();
        engine.reload(crate::load_config(&store.path).unwrap());
        assert_eq!(
            store.view(&engine).unwrap().operations[0].active_local,
            ["kev", "jev"]
        );
    }
    #[actix_web::test]
    async fn settings_http_returns_conflicts_and_validated_updates() {
        let (_dir, engine) = fixture();
        let before = engine.settings.as_ref().unwrap().view(&engine).unwrap();
        let app = actix_web::test::init_service(
            actix_web::App::new()
                .app_data(web::Data::new(engine))
                .configure(crate::routes),
        )
        .await;
        let request = actix_web::test::TestRequest::put()
            .uri(SETTINGS_PATH)
            .set_json(update(&before))
            .to_request();
        let result = actix_web::test::call_service(&app, request).await;
        assert!(result.status().is_success());
        let saved: RoutingSettings = actix_web::test::read_body_json(result).await;
        assert!(saved.pending);
        let request = actix_web::test::TestRequest::put()
            .uri(SETTINGS_PATH)
            .set_json(update(&before))
            .to_request();
        assert_eq!(
            actix_web::test::call_service(&app, request).await.status(),
            409
        );
        let request = actix_web::test::TestRequest::get()
            .uri(SETTINGS_PATH)
            .to_request();
        let view: RoutingSettings = actix_web::test::call_and_read_body_json(&app, request).await;
        assert_eq!(view.revision, saved.revision);
    }

    #[test]
    fn settings_reject_ambiguous_alias_thresholds_before_persisting() {
        let (_dir, engine) = fixture();
        let store = engine.settings.as_ref().unwrap();
        let mut config = crate::load_config(&store.path).unwrap();
        config
            .models
            .insert("jev-backup".into(), config.models["jev"].clone());
        std::fs::write(&store.path, serde_yaml::to_string(&config).unwrap()).unwrap();
        let before = store.view(&engine).unwrap();
        let mut change = update(&before);
        change.routing.cloud.backup = Some("jev-backup".into());
        change.thresholds_by_model.insert(
            "jev-backup".into(),
            BTreeMap::from([("apply".into(), 0.95)]),
        );
        let error = store.update(&engine, change.clone()).unwrap_err();
        assert_eq!(error.0, 400);
        assert!(error.1.contains("same thresholds"));
        assert_eq!(store.view(&engine).unwrap().revision, before.revision);
        change.thresholds_by_model.insert(
            "jev-backup".into(),
            change.thresholds_by_model["jev"].clone(),
        );
        assert!(store.update(&engine, change).is_ok());
    }
}
