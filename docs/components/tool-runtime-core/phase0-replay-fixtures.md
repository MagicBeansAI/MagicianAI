# Tool Runtime Phase 0C Replay Fixtures

This generated report freezes the credential-free invocation shape of every current schema action. It records executable identity, exact static argv, typed argument mappings, safe environment names, profile policy, approval, timeout, and output class. It never reads or records credential values, environment values, parameter defaults, auth command bodies, descriptions, or adapter contents. Production dispatch is unchanged.

## Summary

- Schema version: `tool-runtime.replay-fixtures.v1`
- Replay fixtures: 451
- Google Workspace fixtures: 98
- Primitive process fixtures: 441
- Command process fixtures: 0
- Compiled-provider fixtures: 0
- Official MCP SDK fixtures: 10
- Fixtures accepting stdin: 146
- Declared environment bindings: 196
- Explicitly recorded ignored legacy fields: 0

## Skills

| Skill | Fixtures | Dispatch | Approval | Profile | Required env | Ignored legacy fields |
|---|---:|---|---|---|---:|---:|
| `agentmail-read` | 8 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `agentmail-send` | 3 | `primitive_process` | `conditional_external_side_effect` | `none` / `none` | 0 | 0 |
| `analyze-image-via-minimax` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `arxiv-search` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `awk` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `browser` | 57 | `primitive_process` | `conditional_external_side_effect` | `selectable` / `runtime_named` | 0 | 0 |
| `calendar` | 13 | `primitive_process` | `conditional_external_side_effect` | `selectable` / `schema_selector` | 0 | 0 |
| `csvkit` | 26 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `deep-research-with-claude` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `deep-research-with-openai` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `document-to-markdown` | 2 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `dugite` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `find-details-by-username` | 4 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `gif-search-via-klipy` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `github-search` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `gmail` | 18 | `primitive_process` | `conditional_external_side_effect` | `selectable` / `schema_selector` | 0 | 0 |
| `hackernews-search` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `higgsfield` | 1 | `primitive_process` | `ordinary` | `implicit` / `cli_active_identity` | 0 | 0 |
| `htmltotext` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `image-generation` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `image-generation-via-minimax` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `jq` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `kapso-whatsapp-read` | 7 | `primitive_process` | `ordinary` | `fixed` / `fixed_binding` | 0 | 0 |
| `kapso-whatsapp-send` | 1 | `primitive_process` | `conditional_external_side_effect` | `fixed` / `fixed_binding` | 0 | 0 |
| `macos-ui-automation` | 1 | `primitive_process` | `native_ui_control` | `none` / `none` | 0 | 0 |
| `marimo` | 20 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `meme-generation-via-imgflip` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 2 | 0 |
| `metabase` | 101 | `primitive_process` | `conditional_external_side_effect` | `none` / `none` | 2 | 0 |
| `music-generation-via-minimax` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `news-search-via-tavily` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `ocr` | 3 | `primitive_process` | `ordinary` | `implicit` / `cli_active_identity` | 0 | 0 |
| `office-excel` | 14 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `office-powerpoint` | 15 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `office-word` | 15 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `pdftotext` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `polymarket-search` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `presto-calendar` | 13 | `primitive_process` | `conditional_external_side_effect` | `fixed` / `fixed_binding` | 0 | 0 |
| `presto-gmail` | 18 | `primitive_process` | `conditional_external_side_effect` | `fixed` / `fixed_binding` | 0 | 0 |
| `presto-sheets` | 18 | `primitive_process` | `conditional_external_side_effect` | `fixed` / `fixed_binding` | 0 | 0 |
| `producthunt-search` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `reddit-search` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 2 | 0 |
| `rg` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `screen-draw` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `sed` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `semantic-websearch-via-exa` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `sheets` | 18 | `primitive_process` | `conditional_external_side_effect` | `selectable` / `schema_selector` | 0 | 0 |
| `structured-web-data` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `swiggy-mcp` | 5 | `official_mcp_sdk` | `commerce_checkout` | `selectable` / `schema_selector` | 0 | 0 |
| `telegram` | 2 | `primitive_process` | `conditional_external_side_effect` | `fixed` / `fixed_binding` | 1 | 0 |
| `telegram-self` | 2 | `primitive_process` | `conditional_external_side_effect` | `implicit` / `session_identity` | 0 | 0 |
| `video-generation-via-minimax` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `video-generation-via-veo` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 2 | 0 |
| `web-search-via-minimax` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `web-via-tinyfish` | 2 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `websearch` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `websearch-via-claude` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `websearch-via-openai` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 1 | 0 |
| `whatsapp` | 3 | `primitive_process` | `conditional_external_side_effect` | `implicit` / `cli_active_identity` | 0 | 0 |
| `whatsgoingon2` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `work-modules` | 23 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `youtube-search` | 1 | `primitive_process` | `ordinary` | `none` / `none` | 0 | 0 |
| `zepto-mcp` | 5 | `official_mcp_sdk` | `commerce_checkout` | `selectable` / `schema_selector` | 0 | 0 |

## Contract notes

- `primitive_process` mirrors the current CLI-template dispatcher: the implementation command (or skill id fallback), action argv/name, action mappings, action suffix, then implementation suffix.
- `command_process` mirrors the current command provider: implementation fixed args, implementation mappings, then implementation suffix. Action-level argv/mapping metadata is listed as ignored when present.
- `compiled_provider` records a typed schema boundary without inventing a subprocess recipe.
- `official_mcp_sdk` records the stable product controls above live SDK discovery without freezing remote provider tool schemas.
- Environment bindings retain only variable names and placeholder names. Literal environment content is represented by a boolean and is never copied.
