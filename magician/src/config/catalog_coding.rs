//! Pi choices are derived from the shared catalog and never authored as rows.

use super::{CodingProfileConfig, MagicianConfig};

pub(super) const CHOICE_PREFIX: &str = "pi/model/";

pub(super) fn install_choices(config: &mut MagicianConfig) -> anyhow::Result<()> {
    let Some(router) = config.router_config() else {
        return Ok(());
    };
    let generated = router
        .catalog
        .choices()
        .into_iter()
        .filter(|choice| router.catalog.models[&choice.id].coding)
        .flat_map(|choice| {
            choice
                .choices
                .into_iter()
                .map(move |(thinking, route)| CodingProfileConfig {
                    id: format!("pi/{route}"),
                    label: Some(choice.label.clone()),
                    llm_profile: route,
                    description: Some(format!("{} · {thinking} thinking", choice.label)),
                    turn_timeout_secs: None,
                    enabled: true,
                })
        })
        .collect::<Vec<_>>();
    config.coding.profiles = generated;
    Ok(())
}

pub(super) fn model_descriptor(
    router: &magicllm::LLMRouterConfig,
    selector: &str,
    selection: &magicllm::catalog::ModelSelection,
) -> Option<magicllm::catalog::ModelChoice> {
    if !selector.starts_with(CHOICE_PREFIX) {
        return None;
    }
    let mut choice = router.catalog.choices().into_iter().find(|choice| {
        choice.id == selection.model_id
            && router.catalog.models[&choice.id].coding
            && choice
                .choices
                .get("auto")
                .is_some_and(|route| selector == format!("pi/{route}"))
    })?;
    choice
        .choices
        .values_mut()
        .for_each(|route| *route = format!("pi/{route}"));
    // Pi's SDK can apply a narrower context/compaction limit than the API
    // catalog. Until its runtime attests that limit, do not advertise it as
    // the coding engine's usable capacity.
    choice.limits = None;
    Some(choice)
}
