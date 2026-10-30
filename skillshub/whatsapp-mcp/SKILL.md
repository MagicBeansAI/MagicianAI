---
name: whatsapp-mcp
version: 0.1.0
description: WhatsApp via upstream wu-cli 0.3.1 native MCP tools. Prefer this for
  messages, media, search/history, exports, groups and constraints; use whatsapp CLI
  companion only for missing MCP capabilities.
metadata:
  magician:
    setup:
      definition: whatsapp
    requires:
      bins:
      - wu
      - node
    install_hint:
      docs: Shares the wu CLI and Node runtime of the whatsapp skill; set it up and log
        in via QR code with the whatsapp login command.
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: Authenticated account operations; verified offline without contacting
          WhatsApp.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - wu
        - node
      runtime:
        protocol: mcp
        transport:
          kind: stdio
          executable: wu
          args:
          - mcp
        discovery:
          namespace: whatsapp
          tool_policies:
            wu_messages_send:
              risk: external_side_effect
            wu_react:
              risk: external_side_effect
            wu_media_download:
              risk: workspace_write
            wu_groups_create:
              risk: external_side_effect
            wu_groups_leave:
              risk: external_side_effect
            wu_messages_search:
              risk: read_only
            wu_chats_list:
              risk: read_only
            wu_messages_list:
              risk: read_only
            wu_contacts_list:
              risk: read_only
            wu_status:
              risk: read_only
            wu_chats_search:
              risk: read_only
            wu_contacts_search:
              risk: read_only
            wu_groups_list:
              risk: read_only
            wu_communities_list:
              risk: read_only
            wu_dms_list:
              risk: read_only
            wu_groups_info:
              risk: read_only
            wu_groups_invite:
              risk: read_only
            wu_messages_context:
              risk: read_only
            wu_messages_count:
              risk: read_only
            wu_messages_export:
              risk: workspace_write
            wu_history_backfill:
              risk: workspace_write
            wu_media_download_batch:
              risk: workspace_write
            wu_media_prune:
              risk: external_side_effect
            wu_enrich_status:
              risk: read_only
            wu_media_transcribe:
              risk: workspace_write
            wu_media_ocr:
              risk: workspace_write
            wu_groups_rename:
              risk: external_side_effect
            wu_groups_join:
              risk: external_side_effect
            wu_constraints_list:
              risk: read_only
            wu_constraints_set:
              risk: external_side_effect
            wu_constraints_remove:
              risk: external_side_effect
            wu_constraints_default:
              risk: external_side_effect
            wu_config_show:
              risk: read_only
            wu_sync_pull:
              risk: external_side_effect
            wu_db_reset:
              risk: external_side_effect
        limits:
          timeout_secs: 600
          stdout_bytes: 33554432
          stderr_bytes: 1048576
      auth:
        kind: cli_profile
        requirement: required
        provider: whatsapp
        profile_selection:
          mode: implicit
        storage:
          kind: cli_owned
      policy_floor:
        approval: ordinary
        resource_scopes:
        - workspace
    mcp_bindings:
      environment:
        WU_HOME: '{home}/.wu'
      tools:
        wu_messages_send:
          outward:
            channel: whatsapp
            provider: whatsapp
            recipients:
            - /to
            audience_optional: false
            receipt: /id
            tracks_delivery: true
            consequence_class: bounded_communication
          paths:
            /media_path: read_file
        wu_react:
          outward:
            channel: whatsapp
            provider: whatsapp
            recipients:
            - /chat
            audience_optional: false
            consequence_class: bounded_communication
        wu_media_download:
          paths:
            /out_dir: create_directory
          artifacts:
          - pointer: /path
            root: home/.wu/media
        wu_groups_create:
          outward:
            channel: form
            provider: whatsapp
            recipients:
            - /participants
            audience_optional: false
        wu_groups_leave:
          outward:
            channel: form
            provider: whatsapp
            recipients:
            - /jid
            audience_optional: false
        wu_messages_search: {}
        wu_chats_list: {}
        wu_messages_list: {}
        wu_contacts_list: {}
        wu_status: {}
        wu_chats_search: {}
        wu_contacts_search: {}
        wu_groups_list: {}
        wu_communities_list: {}
        wu_dms_list: {}
        wu_groups_info: {}
        wu_groups_invite: {}
        wu_messages_context: {}
        wu_messages_count: {}
        wu_messages_export:
          artifacts:
          - pointer: /file
            root: home/.wu/exports
          - pointer: /manifest_file
            root: home/.wu/exports
        wu_history_backfill: {}
        wu_media_download_batch:
          artifacts:
          - pointer: /results/*/path
            root: home/.wu/media
        wu_media_prune: {}
        wu_enrich_status: {}
        wu_media_transcribe: {}
        wu_media_ocr: {}
        wu_groups_rename:
          outward:
            channel: form
            provider: whatsapp
            recipients:
            - /jid
            audience_optional: false
        wu_groups_join:
          outward:
            channel: form
            provider: whatsapp
            recipients: []
            audience_optional: true
        wu_constraints_list: {}
        wu_constraints_set: {}
        wu_constraints_remove: {}
        wu_constraints_default: {}
        wu_config_show:
          redact_fields:
          - api_key
          - apiKey
          - password
          - token
          - secret
          - credentials
        wu_sync_pull: {}
        wu_db_reset: {}
    runtime_catalog:
      categories:
      - communication
      - messaging
      composition_category: communication_operations
---

# WhatsApp MCP

Use `list_tools`, then `call_tool` with the native name and JSON arguments. Schemas
come directly from pinned upstream wu; local policy controls risk and approvals.
Text, attachments, reactions, history/context/counts, exports, downloads, OCR,
transcription, groups and constraints use upstream semantics. Polls and CLI-only
maintenance use the `whatsapp` companion. `run` is not restricted to reads.

Authentication and the shared daemon socket stay in this scope's `WU_HOME`.
The bot owns the connection; MCP calls reuse its socket. Reads respect wu chat
constraints. Relative file inputs use the task workdir (scope workdirs outside a
task); explicit absolute paths may name current-task or scope-workdirs files.
All file inputs retain the shared containment and symlink checks;
exports use wu's own confined exports directory under `home/.wu/exports`.
Outward calls pass suppression, capture and durable disclosure/receipt controls.
A provider id proves acceptance, not delivery. Never retry an uncertain send.
Changing collection constraints requires approval. Newly discovered stdio tools
need reviewed local argument bindings; remote annotations cannot grant authority.
