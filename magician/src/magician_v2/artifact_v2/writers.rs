use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use serde_json::Value;

use super::{service::ArtifactV2Error, workspace::ArtifactV2Workspace};

#[derive(Debug, Clone)]
pub enum OutputBody {
    Text(String),
    Json(Value),
    /// Immutable, workspace-owned bytes already accepted under a stable
    /// digest. Writers stream this source to the destination and re-check the
    /// digest before atomic publication; clones retain only Arc handles.
    File {
        source_path: Arc<PathBuf>,
        expected_sha256: Arc<str>,
        /// Optional bounded UTF-8 prefix for auxiliary voice/UI metadata.
        /// It is never authoritative output content.
        text_preview: Option<Arc<str>>,
    },
}

#[derive(Debug, Clone)]
pub struct OutputDocument {
    pub media_type: String,
    pub body: OutputBody,
}

#[derive(Debug, Clone)]
pub struct PersistedOutput {
    pub media_type: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputClass {
    ExecutionAgent,
    TaskAgent,
    TaskUser,
}

#[async_trait]
pub trait OutputWriter: Send + Sync {
    fn writer_id(&self) -> &'static str;

    fn supported_media_types(&self) -> Vec<&'static str>;

    fn supports(&self, media_type: &str) -> bool {
        let base_type = base_media_type(media_type);
        self.supported_media_types()
            .into_iter()
            .any(|supported| supported == base_type)
    }

    fn validate(&self, document: &OutputDocument) -> Result<(), ArtifactV2Error>;

    async fn persist(
        &self,
        workspace: &ArtifactV2Workspace,
        output_id: &str,
        output_dir: &Path,
        relative_dir: &str,
        document: &OutputDocument,
    ) -> Result<PersistedOutput, ArtifactV2Error>;
}

pub trait AllowedOutputFormatsPolicy: Send + Sync {
    fn allowed_media_types(
        &self,
        output_class: OutputClass,
        registry: &OutputWriterRegistry,
    ) -> Vec<String>;

    fn preferred_media_type(
        &self,
        output_class: OutputClass,
        allowed_media_types: &[String],
    ) -> Option<String>;
}

#[derive(Clone, Default)]
pub struct OutputWriterRegistry {
    writers: Vec<Arc<dyn OutputWriter>>,
}

impl OutputWriterRegistry {
    pub fn new() -> Self {
        Self {
            writers: Vec::new(),
        }
    }

    pub fn with_default_text_writers() -> Self {
        Self::new().register(Arc::new(TextOutputWriter))
    }

    pub fn register(mut self, writer: Arc<dyn OutputWriter>) -> Self {
        self.writers.push(writer);
        self
    }

    pub fn supports(&self, media_type: &str) -> bool {
        self.writers
            .iter()
            .any(|writer| writer.supports(media_type))
    }

    pub fn supported_media_types(&self) -> Vec<String> {
        let mut media_types = Vec::new();
        for writer in &self.writers {
            for media_type in writer.supported_media_types() {
                if !media_types.iter().any(|existing| existing == media_type) {
                    media_types.push(media_type.to_string());
                }
            }
        }
        media_types
    }

    pub async fn persist(
        &self,
        workspace: &ArtifactV2Workspace,
        output_id: &str,
        output_dir: &Path,
        relative_dir: &str,
        document: &OutputDocument,
    ) -> Result<PersistedOutput, ArtifactV2Error> {
        let media_type = base_media_type(&document.media_type);
        let writer = self
            .writers
            .iter()
            .find(|writer| writer.supports(media_type))
            .ok_or_else(|| {
                ArtifactV2Error::InvalidRequest(format!(
                    "unsupported_output_media_type:{media_type}"
                ))
            })?;
        writer.validate(document)?;
        writer
            .persist(workspace, output_id, output_dir, relative_dir, document)
            .await
    }
}

pub struct DefaultAllowedOutputFormatsPolicy;

impl AllowedOutputFormatsPolicy for DefaultAllowedOutputFormatsPolicy {
    fn allowed_media_types(
        &self,
        output_class: OutputClass,
        registry: &OutputWriterRegistry,
    ) -> Vec<String> {
        // Task user-outputs are the dashboard payload — the LLM should be
        // free to pick whichever format fits the content. `application/json`
        // unlocks MUI-JSON dashboards with live data_source bindings; XML is
        // for the rare case where the source data is XML and we want the
        // collapsible viewer. Markdown stays the typical default (the
        // synthesis prompt v1.1.0+ teaches the LLM when to pick each).
        let candidates: &[&str] = match output_class {
            OutputClass::ExecutionAgent => &["application/json", "text/markdown", "text/plain"],
            OutputClass::TaskAgent => &["application/json", "text/markdown"],
            OutputClass::TaskUser => &[
                "application/json",
                "text/markdown",
                "text/html",
                "text/plain",
                "application/xml",
                "text/xml",
            ],
        };

        candidates
            .iter()
            .filter(|media_type| registry.supports(media_type))
            .map(|media_type| (*media_type).to_string())
            .collect()
    }

    fn preferred_media_type(
        &self,
        output_class: OutputClass,
        allowed_media_types: &[String],
    ) -> Option<String> {
        let preferred = match output_class {
            OutputClass::ExecutionAgent => "text/markdown",
            OutputClass::TaskAgent => "application/json",
            // HTML is the default for user-facing deliverables (synthesis
            // prompt v1.3.0+); markdown is reserved for rudimentary output and
            // JSON for data-rich dashboards. This is the fallback hint when the
            // synthesizer doesn't pin a media_type itself.
            OutputClass::TaskUser => "text/html",
        };

        if allowed_media_types.iter().any(|value| value == preferred) {
            Some(preferred.to_string())
        } else {
            allowed_media_types.first().cloned()
        }
    }
}

pub struct TextOutputWriter;

#[async_trait]
impl OutputWriter for TextOutputWriter {
    fn writer_id(&self) -> &'static str {
        "text_output_writer"
    }

    fn supported_media_types(&self) -> Vec<&'static str> {
        vec![
            "application/json",
            "text/markdown",
            "text/html",
            "text/plain",
            "application/xml",
            "text/xml",
        ]
    }

    fn validate(&self, document: &OutputDocument) -> Result<(), ArtifactV2Error> {
        let media_type = base_media_type(&document.media_type);
        match (media_type, &document.body) {
            ("application/json", OutputBody::Json(_)) => Ok(()),
            ("application/json", OutputBody::Text(_)) => Err(ArtifactV2Error::InvalidRequest(
                "output_document_json_requires_body_json".to_string(),
            )),
            ("application/json", OutputBody::File { .. }) => {
                validate_file_body_authority(&document.body)
            },
            ("text/markdown", OutputBody::Text(_))
            | ("text/html", OutputBody::Text(_))
            | ("text/plain", OutputBody::Text(_))
            | ("application/xml", OutputBody::Text(_))
            | ("text/xml", OutputBody::Text(_)) => Ok(()),
            (
                "text/markdown" | "text/html" | "text/plain" | "application/xml" | "text/xml",
                OutputBody::File { .. },
            ) => validate_file_body_authority(&document.body),
            (
                "text/markdown" | "text/html" | "text/plain" | "application/xml" | "text/xml",
                OutputBody::Json(_),
            ) => Err(ArtifactV2Error::InvalidRequest(format!(
                "output_document_text_requires_body_text:{media_type}"
            ))),
            (unsupported, _) => Err(ArtifactV2Error::InvalidRequest(format!(
                "unsupported_output_media_type:{unsupported}"
            ))),
        }
    }

    async fn persist(
        &self,
        workspace: &ArtifactV2Workspace,
        output_id: &str,
        output_dir: &Path,
        relative_dir: &str,
        document: &OutputDocument,
    ) -> Result<PersistedOutput, ArtifactV2Error> {
        let media_type = base_media_type(&document.media_type).to_string();
        let extension = extension_for_media_type(&media_type)?;
        let file_name = format!("{output_id}.{extension}");
        let full_path = output_dir.join(&file_name);
        match &document.body {
            OutputBody::Text(body) => workspace.write_string_atomic_path(&full_path, body).await?,
            OutputBody::Json(value) => workspace.write_json_atomic_path(&full_path, value).await?,
            OutputBody::File {
                source_path,
                expected_sha256,
                ..
            } => {
                if media_type == "application/json" {
                    workspace
                        .validate_workspace_json_file_path(source_path.as_ref())
                        .await?;
                }
                workspace
                    .copy_workspace_file_verified_atomic_path(
                        source_path.as_ref(),
                        &full_path,
                        expected_sha256.as_ref(),
                    )
                    .await?;
            },
        }
        Ok(PersistedOutput {
            media_type,
            relative_path: format!("{relative_dir}/{file_name}"),
        })
    }
}

fn validate_file_body_authority(body: &OutputBody) -> Result<(), ArtifactV2Error> {
    let OutputBody::File {
        source_path,
        expected_sha256,
        ..
    } = body
    else {
        return Err(ArtifactV2Error::InvalidRequest(
            "output_document_file_authority_missing".to_string(),
        ));
    };
    if source_path.as_os_str().is_empty() {
        return Err(ArtifactV2Error::InvalidRequest(
            "output_document_file_source_missing".to_string(),
        ));
    }
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ArtifactV2Error::InvalidRequest(
            "output_document_file_digest_invalid".to_string(),
        ));
    }
    Ok(())
}

pub fn base_media_type(media_type: &str) -> &str {
    media_type
        .split(';')
        .next()
        .map(str::trim)
        .unwrap_or(media_type)
}

fn extension_for_media_type(media_type: &str) -> Result<&'static str, ArtifactV2Error> {
    match media_type {
        "application/json" => Ok("json"),
        "text/markdown" => Ok("md"),
        "text/html" => Ok("html"),
        "text/plain" => Ok("txt"),
        "application/xml" | "text/xml" => Ok("xml"),
        unsupported => Err(ArtifactV2Error::InvalidRequest(format!(
            "unsupported_output_media_type:{unsupported}"
        ))),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    #[tokio::test]
    async fn output_writer_persists_inside_workspace_provider() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let output_dir = workspace.base_root().join("outputs");
        let registry = OutputWriterRegistry::with_default_text_writers();
        let document = OutputDocument {
            media_type: "text/markdown; charset=utf-8".to_string(),
            body: OutputBody::Text("# Done\n".to_string()),
        };

        let persisted = registry
            .persist(&workspace, "final", &output_dir, "outputs", &document)
            .await
            .expect("output should persist inside provider root");

        assert_eq!(persisted.media_type, "text/markdown");
        assert_eq!(persisted.relative_path, "outputs/final.md");
        let body = workspace
            .read_to_string_path(output_dir.join("final.md"))
            .await
            .expect("persisted output should be readable through provider");
        assert_eq!(body, "# Done\n");
    }

    #[tokio::test]
    async fn output_writer_rejects_paths_outside_workspace_provider() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let escaped_output_dir = workspace.base_root().join("../outside");
        let registry = OutputWriterRegistry::with_default_text_writers();
        let document = OutputDocument {
            media_type: "application/json".to_string(),
            body: OutputBody::Json(serde_json::json!({ "ok": true })),
        };

        let error = registry
            .persist(
                &workspace,
                "final",
                &escaped_output_dir,
                "outputs",
                &document,
            )
            .await
            .expect_err("provider should reject parent traversal");

        assert!(matches!(error, ArtifactV2Error::InvalidRequest(_)));
    }

    #[tokio::test]
    async fn file_backed_output_shares_authority_and_copies_large_bytes_exactly() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let source = workspace.base_root().join("executions/source.md");
        let output_dir = workspace.base_root().join("outputs");
        let body = "accepted terminal bytes\n".repeat(192 * 1024).into_bytes();
        workspace
            .write_atomic_path(&source, &body)
            .await
            .expect("large source");
        let digest = Arc::<str>::from(format!("{:x}", Sha256::digest(&body)));
        let source = Arc::new(source);
        let preview = Arc::<str>::from("accepted terminal bytes");
        let document = OutputDocument {
            media_type: "text/markdown".to_string(),
            body: OutputBody::File {
                source_path: Arc::clone(&source),
                expected_sha256: Arc::clone(&digest),
                text_preview: Some(Arc::clone(&preview)),
            },
        };
        let cloned = document.clone();
        let OutputBody::File {
            source_path: cloned_source,
            expected_sha256: cloned_digest,
            text_preview: Some(cloned_preview),
        } = &cloned.body
        else {
            panic!("file-backed clone");
        };
        assert!(Arc::ptr_eq(&source, cloned_source));
        assert!(Arc::ptr_eq(&digest, cloned_digest));
        assert!(Arc::ptr_eq(&preview, cloned_preview));

        OutputWriterRegistry::with_default_text_writers()
            .persist(&workspace, "final", &output_dir, "outputs", &document)
            .await
            .expect("verified streaming copy");
        let copied = workspace
            .read_path(output_dir.join("final.md"))
            .await
            .expect("copied output");
        assert_eq!(copied, body);
    }

    #[tokio::test]
    async fn file_backed_output_rejects_external_or_changed_sources() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let output_dir = workspace.base_root().join("outputs");
        let external = temp.path().join("external.md");
        tokio::fs::write(&external, b"outside")
            .await
            .expect("external fixture");
        let external_document = OutputDocument {
            media_type: "text/markdown".to_string(),
            body: OutputBody::File {
                source_path: Arc::new(external),
                expected_sha256: Arc::<str>::from(format!("{:x}", Sha256::digest(b"outside"))),
                text_preview: None,
            },
        };
        let registry = OutputWriterRegistry::with_default_text_writers();
        assert!(registry
            .persist(
                &workspace,
                "external",
                &output_dir,
                "outputs",
                &external_document,
            )
            .await
            .is_err());

        #[cfg(unix)]
        {
            let linked = workspace.base_root().join("executions/linked-external.md");
            std::fs::create_dir_all(linked.parent().expect("linked source parent"))
                .expect("linked source parent");
            std::os::unix::fs::symlink(temp.path().join("external.md"), &linked)
                .expect("external symlink fixture");
            let linked_document = OutputDocument {
                media_type: "text/markdown".to_string(),
                body: OutputBody::File {
                    source_path: Arc::new(linked),
                    expected_sha256: Arc::<str>::from(format!("{:x}", Sha256::digest(b"outside"))),
                    text_preview: None,
                },
            };
            assert!(registry
                .persist(
                    &workspace,
                    "linked-external",
                    &output_dir,
                    "outputs",
                    &linked_document,
                )
                .await
                .is_err());
        }

        let source = workspace.base_root().join("executions/changed.md");
        workspace
            .write_atomic_path(&source, b"accepted")
            .await
            .expect("accepted source");
        let changed_document = OutputDocument {
            media_type: "text/markdown".to_string(),
            body: OutputBody::File {
                source_path: Arc::new(source.clone()),
                expected_sha256: Arc::<str>::from(format!("{:x}", Sha256::digest(b"accepted"))),
                text_preview: None,
            },
        };
        workspace
            .write_atomic_path(&source, b"tampered")
            .await
            .expect("mutate after acceptance");
        assert!(registry
            .persist(
                &workspace,
                "changed",
                &output_dir,
                "outputs",
                &changed_document,
            )
            .await
            .is_err());
        assert!(workspace
            .symlink_metadata_path(output_dir.join("changed.md"))
            .await
            .expect("destination metadata")
            .is_none());
    }

    #[tokio::test]
    async fn file_backed_json_preserves_streaming_media_validation() {
        let temp = tempdir().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp.path().join("magician_data_v3"));
        let output_dir = workspace.base_root().join("outputs");
        let registry = OutputWriterRegistry::with_default_text_writers();
        let valid = workspace.base_root().join("executions/valid.json");
        let valid_bytes = b"{\n  \"accepted\": true,\n  \"details\": [1, 2, 3]\n}\n";
        workspace
            .write_atomic_path(&valid, valid_bytes)
            .await
            .expect("valid JSON source");
        let valid_document = OutputDocument {
            media_type: "application/json".to_string(),
            body: OutputBody::File {
                source_path: Arc::new(valid),
                expected_sha256: Arc::<str>::from(format!("{:x}", Sha256::digest(valid_bytes))),
                text_preview: None,
            },
        };
        registry
            .persist(&workspace, "valid", &output_dir, "outputs", &valid_document)
            .await
            .expect("valid file-backed JSON");
        assert_eq!(
            workspace
                .read_path(output_dir.join("valid.json"))
                .await
                .expect("valid JSON destination"),
            valid_bytes
        );

        let invalid = workspace.base_root().join("executions/invalid.json");
        let invalid_bytes = b"{\"accepted\": true";
        workspace
            .write_atomic_path(&invalid, invalid_bytes)
            .await
            .expect("invalid JSON source");
        let invalid_document = OutputDocument {
            media_type: "application/json".to_string(),
            body: OutputBody::File {
                source_path: Arc::new(invalid),
                expected_sha256: Arc::<str>::from(format!("{:x}", Sha256::digest(invalid_bytes))),
                text_preview: None,
            },
        };

        assert!(registry
            .persist(
                &workspace,
                "invalid",
                &output_dir,
                "outputs",
                &invalid_document,
            )
            .await
            .is_err());
        assert!(workspace
            .symlink_metadata_path(output_dir.join("invalid.json"))
            .await
            .expect("invalid destination metadata")
            .is_none());

        let deep = workspace.base_root().join("executions/deep.json");
        let mut deep_bytes = vec![b'['; 1024];
        deep_bytes.extend(std::iter::repeat_n(b']', 1024));
        workspace
            .write_atomic_path(&deep, &deep_bytes)
            .await
            .expect("deep JSON source");
        let deep_document = OutputDocument {
            media_type: "application/json".to_string(),
            body: OutputBody::File {
                source_path: Arc::new(deep),
                expected_sha256: Arc::<str>::from(format!("{:x}", Sha256::digest(&deep_bytes))),
                text_preview: None,
            },
        };

        assert!(registry
            .persist(&workspace, "deep", &output_dir, "outputs", &deep_document,)
            .await
            .is_err());
        assert!(workspace
            .symlink_metadata_path(output_dir.join("deep.json"))
            .await
            .expect("deep destination metadata")
            .is_none());
    }
}
