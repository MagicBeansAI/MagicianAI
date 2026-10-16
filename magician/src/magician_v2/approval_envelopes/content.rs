//! Exact owner-approved rendering and source provenance for bounded content.
//! This validator grants no dispatch authority and uses no model classification.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
#[path = "content_tests.rs"]
pub(super) mod tests;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TemplatePart {
    Literal { value: String },
    Slot { id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SlotRule {
    FixedChoices {
        values: BTreeSet<String>,
        maximum_bytes: u64,
    },
    SealedSource {
        source_id: String,
        content_hash: String,
        /// None selects the complete UTF-8 source; Some selects a JSON string.
        json_pointer: Option<String>,
        maximum_bytes: u64,
    },
}

/// Files are sealed artifact names, never paths on the actor's filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateContract {
    pub payload_file: String,
    pub slots_file: String,
    pub parts: Vec<TemplatePart>,
    pub slots: BTreeMap<String, SlotRule>,
    pub permitted_attachments: BTreeMap<String, BTreeSet<String>>,
    pub maximum_payload_bytes: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SlotInstance {
    #[serde(deserialize_with = "distinct_slots")]
    slots: BTreeMap<String, String>,
}

fn distinct_slots<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    struct Slots;
    impl<'de> serde::de::Visitor<'de> for Slots {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("distinct named string slots")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((id, value)) = map.next_entry::<String, String>()? {
                if values.insert(id, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate template slot"));
                }
            }
            Ok(values)
        }
    }
    decoder.deserialize_map(Slots)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedTemplate {
    pub template_hash: String,
    pub slots_hash: String,
    pub payload_hash: String,
    pub source_hashes: BTreeMap<String, String>,
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_name(value: &str) -> bool {
    !value.trim().is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

impl TemplateContract {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            valid_name(&self.payload_file)
                && valid_name(&self.slots_file)
                && self.payload_file != self.slots_file
                && !self.parts.is_empty()
                && self.maximum_payload_bytes > 0,
            "template needs distinct artifact files and a positive payload bound"
        );
        let mut used = BTreeSet::new();
        for part in &self.parts {
            match part {
                TemplatePart::Literal { value } => ensure!(
                    value.len() as u64 <= self.maximum_payload_bytes,
                    "template literal exceeds payload bound"
                ),
                TemplatePart::Slot { id } => {
                    ensure!(
                        valid_name(id) && self.slots.contains_key(id),
                        "template references an undeclared slot"
                    );
                    used.insert(id);
                },
            }
        }
        ensure!(
            used.len() == self.slots.len(),
            "every slot must occur in the owner-approved template"
        );
        let mut source_pins = BTreeMap::new();
        for rule in self.slots.values() {
            if let SlotRule::SealedSource {
                source_id,
                content_hash,
                ..
            } = rule
            {
                ensure!(
                    source_pins
                        .insert(source_id, content_hash)
                        .is_none_or(|previous| previous == content_hash),
                    "one template source cannot have conflicting hashes"
                );
            }
            match rule {
                SlotRule::FixedChoices {
                    values,
                    maximum_bytes,
                } => ensure!(
                    *maximum_bytes > 0
                        && *maximum_bytes <= self.maximum_payload_bytes
                        && !values.is_empty()
                        && values
                            .iter()
                            .all(|value| value.len() as u64 <= *maximum_bytes),
                    "slot needs finite choices within its byte bound"
                ),
                SlotRule::SealedSource {
                    source_id,
                    content_hash,
                    json_pointer,
                    maximum_bytes,
                } => ensure!(
                    valid_name(source_id)
                        && valid_hash(content_hash)
                        && *maximum_bytes > 0
                        && *maximum_bytes <= self.maximum_payload_bytes
                        && json_pointer
                            .as_ref()
                            .is_none_or(|pointer| pointer.is_empty() || pointer.starts_with('/')),
                    "source slot needs an exact source hash and JSON pointer"
                ),
            }
        }
        ensure!(
            self.permitted_attachments
                .iter()
                .all(|(file, hashes)| valid_name(file)
                    && file != &self.payload_file
                    && file != &self.slots_file
                    && !hashes.is_empty()
                    && hashes.iter().all(|hash| valid_hash(hash))),
            "attachments need exact owner-approved hashes"
        );
        Ok(())
    }

    pub fn hash(&self) -> Result<String> {
        self.validate()?;
        Ok(blake3::hash(&serde_json::to_vec(self)?)
            .to_hex()
            .to_string())
    }

    pub fn validate_proof(
        &self,
        proof: &ValidatedTemplate,
        files: &BTreeMap<String, String>,
    ) -> Result<()> {
        let sources: BTreeMap<_, _> = self
            .slots
            .values()
            .filter_map(|rule| match rule {
                SlotRule::SealedSource {
                    source_id,
                    content_hash,
                    ..
                } => Some((source_id.clone(), content_hash.clone())),
                SlotRule::FixedChoices { .. } => None,
            })
            .collect();
        ensure!(
            proof.template_hash == self.hash()?
                && proof.source_hashes == sources
                && files.get(&self.payload_file) == Some(&proof.payload_hash)
                && files.get(&self.slots_file) == Some(&proof.slots_hash)
                && valid_hash(&proof.payload_hash)
                && valid_hash(&proof.slots_hash)
                && files.iter().all(|(file, hash)| file == &self.payload_file
                    || file == &self.slots_file
                    || self
                        .permitted_attachments
                        .get(file)
                        .is_some_and(|allowed| allowed.contains(hash))),
            "template proof lost its source or artifact binding"
        );
        Ok(())
    }

    /// Verify every byte of the outgoing representation, all declared slots,
    /// permitted source pins and every additional artifact. Novel text does
    /// not become a bounded slot merely because a critic accepted the draft.
    pub fn validate_artifact(
        &self,
        files: &BTreeMap<String, Vec<u8>>,
        sources: &BTreeMap<String, Vec<u8>>,
    ) -> Result<ValidatedTemplate> {
        self.validate()?;
        let payload = files
            .get(&self.payload_file)
            .context("template payload file is missing")?;
        let slots = files
            .get(&self.slots_file)
            .context("template slots file is missing")?;
        ensure!(
            payload.len() as u64 <= self.maximum_payload_bytes,
            "template payload exceeds its byte bound"
        );
        let instance: SlotInstance =
            serde_json::from_slice(slots).context("template slots must be a JSON slots object")?;
        ensure!(
            instance.slots.keys().eq(self.slots.keys()),
            "template instance changed the declared slots"
        );
        let mut source_hashes = BTreeMap::new();
        for (id, rule) in &self.slots {
            let value = &instance.slots[id];
            match rule {
                SlotRule::FixedChoices {
                    values,
                    maximum_bytes,
                } => ensure!(
                    value.len() as u64 <= *maximum_bytes && values.contains(value),
                    "slot {id} is outside its owner-approved choices"
                ),
                SlotRule::SealedSource {
                    source_id,
                    content_hash,
                    json_pointer,
                    maximum_bytes,
                } => {
                    let bytes = sources
                        .get(source_id)
                        .with_context(|| format!("slot {id} source is unavailable"))?;
                    ensure!(
                        blake3::hash(bytes).to_hex().as_str() == content_hash,
                        "slot {id} source changed"
                    );
                    let expected = if let Some(pointer) = json_pointer {
                        let source: serde_json::Value =
                            serde_json::from_slice(bytes).context("slot source is not JSON")?;
                        source
                            .pointer(pointer)
                            .and_then(serde_json::Value::as_str)
                            .context("slot source pointer does not name a string")?
                            .to_owned()
                    } else {
                        std::str::from_utf8(bytes)
                            .context("slot source is not UTF-8")?
                            .to_owned()
                    };
                    ensure!(
                        value.len() as u64 <= *maximum_bytes && value == &expected,
                        "slot {id} differs from its sealed source value"
                    );
                    source_hashes.insert(source_id.clone(), content_hash.clone());
                },
            }
        }
        let mut rendered = Vec::new();
        for part in &self.parts {
            let bytes = match part {
                TemplatePart::Literal { value } => value.as_bytes(),
                TemplatePart::Slot { id } => instance.slots[id].as_bytes(),
            };
            let next = rendered
                .len()
                .checked_add(bytes.len())
                .context("template size overflow")?;
            ensure!(
                next as u64 <= self.maximum_payload_bytes,
                "rendered template exceeds payload bound"
            );
            rendered.extend_from_slice(bytes);
        }
        ensure!(
            &rendered == payload,
            "artifact payload differs from the approved template rendering"
        );
        for (file, bytes) in files {
            if file == &self.payload_file || file == &self.slots_file {
                continue;
            }
            let allowed = self
                .permitted_attachments
                .get(file)
                .context("artifact contains an unapproved attachment")?;
            ensure!(
                allowed.contains(blake3::hash(bytes).to_hex().as_str()),
                "artifact attachment bytes changed"
            );
        }
        Ok(ValidatedTemplate {
            template_hash: self.hash()?,
            slots_hash: blake3::hash(slots).to_hex().to_string(),
            payload_hash: blake3::hash(payload).to_hex().to_string(),
            source_hashes,
        })
    }
}
