# Tool Runtime Phase 0A Source Inventory

This report is generated from the canonical schema-backed tool-skill source. It contains only allowlisted identifiers and counts: credential values, lifecycle command bodies, arbitrary defaults, and skill prose are never copied into this artifact.

## Summary

- Schema version: `tool-runtime.source-inventory.v1`
- Activation rule: direct child directory containing one governed SKILL.md runtime package or legacy tool_schema.yaml
- Active tool skills: 62
- Schema actions: 451
- Auth blocks: 38
- Required binary declarations: 94
- Required environment-name declarations: 19
- Injected environment-name declarations: 37
- Adapter/support files: 40
- Profile selectors: 5
- Errors: 0
- Warnings: 0

## Active tool skills

| Skill | Version | Program | Actions | Auth | Required env | Adapters | Profiles |
|---|---:|---|---:|---:|---:|---:|---|
| `agentmail-read` | `0.2.0` | `agentmail` | 8 | yes | 0 | 0 | — |
| `agentmail-send` | `0.2.0` | `agentmail` | 3 | yes | 0 | 0 | — |
| `analyze-image-via-minimax` | `0.2.1` | `minimax-vision` | 1 | yes | 0 | 1 | — |
| `arxiv-search` | `0.2.0` | `arxiv-search` | 1 | no | 0 | 1 | — |
| `awk` | `0.2.0` | `awk` | 1 | no | 0 | 0 | — |
| `browser` | `0.5.0` | `agent-browser` | 57 | yes | 0 | 2 | — |
| `calendar` | `0.1.0` | `gws` | 13 | yes | 0 | 0 | account |
| `csvkit` | `0.2.0` | `in2csv` | 26 | no | 0 | 0 | — |
| `deep-research-with-claude` | `0.2.0` | `claude-deep-research` | 1 | yes | 1 | 1 | — |
| `deep-research-with-openai` | `0.2.0` | `openai-deep-research` | 1 | yes | 1 | 1 | — |
| `document-to-markdown` | `0.1.0` | `document-to-markdown` | 2 | no | 0 | 0 | — |
| `dugite` | `0.2.0` | `git` | 1 | no | 0 | 0 | — |
| `find-details-by-username` | `0.2.0` | `find-details-by-username` | 4 | no | 0 | 1 | — |
| `gif-search-via-klipy` | `0.2.1` | `klipy-gif-search` | 1 | yes | 1 | 1 | — |
| `github-search` | `0.4.0` | `github-search` | 1 | yes | 1 | 1 | — |
| `gmail` | `0.1.0` | `gws` | 18 | yes | 0 | 0 | account |
| `hackernews-search` | `0.2.0` | `hackernews-search` | 1 | no | 0 | 1 | — |
| `higgsfield` | `0.2.0` | `higgsfield` | 1 | yes | 0 | 0 | — |
| `htmltotext` | `0.3.0` | `htmltotext` | 1 | no | 0 | 1 | — |
| `image-generation` | `0.2.0` | `nanobanana2` | 1 | yes | 1 | 1 | — |
| `image-generation-via-minimax` | `0.2.1` | `minimax-image` | 1 | yes | 0 | 1 | — |
| `jq` | `0.2.0` | `jq` | 1 | no | 0 | 0 | — |
| `kapso-whatsapp-read` | `0.2.1` | `kapso-governed` | 7 | yes | 0 | 1 | — |
| `kapso-whatsapp-send` | `0.2.1` | `kapso-governed` | 1 | yes | 0 | 1 | — |
| `macos-ui-automation` | `0.2.0` | `macos-ui-controller` | 1 | yes | 0 | 1 | — |
| `marimo` | `0.2.0` | `marimo` | 20 | no | 0 | 0 | — |
| `meme-generation-via-imgflip` | `0.2.1` | `imgflip-meme` | 1 | yes | 2 | 1 | — |
| `metabase` | `0.2.0` | `metabase-pp-cli` | 101 | yes | 2 | 4 | — |
| `music-generation-via-minimax` | `0.2.1` | `minimax-music` | 1 | yes | 0 | 1 | — |
| `news-search-via-tavily` | `0.2.3` | `tavily-search` | 1 | yes | 1 | 1 | — |
| `ocr` | `0.3.0` | `ocr` | 3 | yes | 0 | 1 | — |
| `office-excel` | `0.2.0` | `officecli` | 14 | no | 0 | 0 | — |
| `office-powerpoint` | `0.2.0` | `officecli` | 15 | no | 0 | 0 | — |
| `office-word` | `0.2.0` | `officecli` | 15 | no | 0 | 0 | — |
| `pdftotext` | `0.2.0` | `pdftotext` | 1 | no | 0 | 0 | — |
| `polymarket-search` | `0.2.0` | `polymarket-search` | 1 | no | 0 | 1 | — |
| `presto-calendar` | `0.1.0` | `gws` | 13 | yes | 0 | 0 | — |
| `presto-gmail` | `0.1.0` | `gws` | 18 | yes | 0 | 0 | — |
| `presto-sheets` | `0.1.0` | `gws` | 18 | yes | 0 | 0 | — |
| `producthunt-search` | `0.3.0` | `producthunt-search` | 1 | no | 0 | 1 | — |
| `reddit-search` | `0.3.1` | `reddit-search` | 1 | yes | 2 | 1 | — |
| `rg` | `0.2.0` | `rg` | 1 | no | 0 | 0 | — |
| `screen-draw` | `0.1.0` | `curl` | 1 | no | 0 | 0 | — |
| `sed` | `0.2.0` | `sed` | 1 | no | 0 | 0 | — |
| `semantic-websearch-via-exa` | `0.2.1` | `exa-search` | 1 | yes | 1 | 1 | — |
| `sheets` | `0.1.0` | `gws` | 18 | yes | 0 | 0 | account |
| `structured-web-data` | `0.2.1` | `structured-web-data` | 1 | no | 0 | 1 | — |
| `swiggy-mcp` | `0.1.0` | `official_mcp_sdk` | 5 | yes | 0 | 0 | profile |
| `telegram` | `0.2.1` | `telegram-bot-adapter` | 2 | yes | 1 | 1 | — |
| `telegram-self` | `0.2.1` | `tgcli` | 2 | yes | 0 | 0 | — |
| `video-generation-via-minimax` | `0.2.1` | `minimax-video` | 1 | yes | 0 | 1 | — |
| `video-generation-via-veo` | `0.2.0` | `veo31` | 1 | yes | 2 | 1 | — |
| `web-search-via-minimax` | `0.2.1` | `minimax-websearch` | 1 | yes | 0 | 1 | — |
| `web-via-tinyfish` | `0.1.0` | `node` | 2 | yes | 1 | 0 | — |
| `websearch` | `0.3.0` | `websearch` | 1 | no | 0 | 1 | — |
| `websearch-via-claude` | `0.2.2` | `claude-websearch` | 1 | yes | 1 | 1 | — |
| `websearch-via-openai` | `0.2.2` | `openai-websearch` | 1 | yes | 1 | 1 | — |
| `whatsapp` | `0.2.1` | `wu` | 3 | yes | 0 | 1 | — |
| `whatsgoingon2` | `0.2.0` | `whatsgoingon2` | 1 | no | 0 | 1 | — |
| `work-modules` | `0.1.0` | `work-modules` | 23 | no | 0 | 1 | — |
| `youtube-search` | `0.3.0` | `youtube-search` | 1 | no | 0 | 1 | — |
| `zepto-mcp` | `0.2.0` | `official_mcp_sdk` | 5 | yes | 0 | 0 | profile |

## Findings

No inventory-contract findings.
