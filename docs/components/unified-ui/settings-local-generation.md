# Settings: on-device generation

Server seam:
`GET`/`PUT /api/magician/v2/settings/local-generation`
([v2 API guide](../magician/v2-api-guide.md#settings),
[local channel LLM](../magician/local-channel-llm.md)).

Settings hosts `$lib/settings/LocalGenerationPanel.svelte` (fed by
`$lib/stores/localGenerationStore.ts`) as **On-device generation**. It lists
the local-generation kitty — Qwen 3.8 27B, Gemma 4 12B, Underdog Woof 4B —
with host RAM, the auto-setup recommendation, catalog scores, and whether
each model is installed in Ollama.

This is the `runtime.ollama.local_generation.selected` pin every Ollama
generation profile reads through the `&local_generation_model` YAML
anchor. It is not per-operation routing; that remains
[model routing](model-routing-panel.md).

Choosing a model and **Switch and reload Ollama** writes the same
`selected: &local_generation_model <id>` line `make setup-ollama` writes,
reloads magician-config, and runs `scripts/run-ollama.sh`. RAM-tier
mismatches (for example Qwen on a 32 GB machine) show **Outside RAM
rule**, ask **Switch anyway**, and still pin. Missing models are not
pulled; the panel says so and still allows the pin. JSON workers stay
`think:false` regardless of which kitty model is selected.

**Reload Ollama** re-runs the launcher for the current pin without
changing it.
