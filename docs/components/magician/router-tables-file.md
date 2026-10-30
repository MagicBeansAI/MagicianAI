# Router catalog beside the main config

`llm-router.yaml`, beside `magician-config.yaml`, owns `llm.router.catalog` and
`operation_mapping`. Connections, model capabilities/limits, workload policies
and assignments are authored here; concrete dispatch routes are generated.
See the [catalog contract](../magicllm/model-catalog.md).

## One configuration pair

A main file and its sibling form one YAML document. Packaging, seeding, copying
and backups must carry both. The live sibling is
`$MAGICIAN_ROOT_DIR/llm-router.yaml`; the repository-root pair is the package seed.
The loader resolves the sibling relative to the selected main file, with no
search fallback to another root. A missing sibling is fatal unless the selected
file already carries inline routing; production still requires a nonempty catalog.

The splice is textual because local catalog models reference
`*local_generation_model`, whose anchor lives under `runtime:` in the main file.
YAML anchors cannot cross independently parsed documents. Settings → On-device
generation surgically changes that selected model and reloads Ollama; it does
not duplicate model literals in the sibling.

## Readers and writers

| Reader | Entry point |
| --- | --- |
| Rust runtime | `load_magician_config_from_path` |
| Rust tests/packaging | `shipped_repo_config_yaml()` / `shipped_template_config_yaml()` |
| Python | `scripts/magician_config_text.py` → `read_config_text(path)` |
| Shell / Ruby | `python3 scripts/magician_config_text.py <config>` |

Read through these owners when routing matters. The Python reader mirrors the
splice and removes the sibling's column-zero header for indentation-based
consumers. It expands a read-only dispatch view and rejects retired authored
profile/adaptive tables and aliases. Production uses the Rust resolver.

Main-config serializers omit the catalog, generated profiles and operation
mapping so unrelated Settings writers cannot copy routing back into the main
file. Edit the sibling as text to retain comments and anchors; never write the
Python dispatch view back as runtime YAML. Update live and seed pairs when
changing configuration defaults.

Install-level [routing overrides](llm-routing-overrides.md) live in their own
owner store. [Chat choices](chat-profile-routing.md) use typed model/thinking
admission and do not edit this configuration pair.
