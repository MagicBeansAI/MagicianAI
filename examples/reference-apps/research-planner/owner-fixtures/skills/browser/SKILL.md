---
name: browser
version: 1.0.0
description: Reviewed source marker for the sealed app-owned Browser physical owner.
metadata:
  magician:
    skill_type: tool
    expose:
      apps: true
      agents: true
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires: { bins: [agent-browser] }
      runtime:
        protocol: cli
        command_prefix: []
        limits:
          timeout_secs: 300
          stdout_bytes: 524288
          stderr_bytes: 4096
    runtime_catalog:
      categories: [browser]
      composition_category: web_operations
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v1
      actions:
        snapshot:
          description: Request one bounded structured observation.
          fixed_args: [snapshot]
---
This reviewed source is classified by the Apps primitive owner as Browser
Interactive. The app selects the owner-built `snapshot`, `navigate`, `scroll`,
and `click` roster as four separate singleton dependencies. The source marker's
CLI action is not ambient authority: none of its broader browser vocabulary
enters an app manifest, grant, or invocation.
