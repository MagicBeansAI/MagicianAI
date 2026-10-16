---
name: browser
version: 0.5.4
description: Drive a browser to accomplish the current task using the pinned agent-browser CLI. The agentic
  loop invokes exact CLI argv commands (open, read, snapshot, click, fill, drag, mouse, eval, screenshot,
  batch), observes the page via read/snapshot/get/eval, and verifies progress from page-owned evidence.
compatibility: Requires the backend runtime — agent-browser binary is deployed via `make setup-agent-browser`.
  Headless/headed engine selection is config-driven. Lightpanda is available as a soft DOM-first headless
  preference; CloakBrowser or bundled Chrome provides full rendering, and CDP remains the user's Chrome.
metadata:
  magician:
    requires:
      bins:
      - agent-browser
    install_hint:
      docs: ships with the runtime — run `make setup-agent-browser` from the repo root
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          the governed action set drives a real browser session against live
          sites and can act as the signed-in operator; there is no isolated
          target to probe safely.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      requires:
        bins:
        - agent-browser
      runtime:
        protocol: cli
        command_prefix: []
        interaction: batch
        stdin:
          mode: optional
          sensitivity: secret
        working_directory:
          mode: denied
        limits:
          timeout_secs: 300
          stdin_bytes: 1048576
          stdout_bytes: 33554432
          stderr_bytes: 4194304
      auth:
        kind: browser_profile
        requirement: conditional
        provider: browser
        profile_selection:
          mode: implicit
        storage:
          kind: browser_profile
      policy_floor:
        approval: ordinary
        resource_scopes:
        - browser
    runtime_actions:
      schema_version: tool-runtime.typed-action-overrides.v2
      actions:
        secure_prompt_fill:
          description: Prompt the user privately through the runtime for one-time credentials and fill the current login form, or deliver material the run already holds (a password or one-time code the user gave through need_user_input, referenced as its placeholder) to the exact origin. Never ask for browser passwords in chat; every value including username stays hidden from the agent. Requires CDP through the configured local browser endpoint, exact HTTPS origin (loopback HTTP allowed), CSS selectors, and final Use once approval (given already when a code was collected for this exact origin). Does not save or submit. A code is used once; a second fill of the same reference is refused. No automatic retry after partial or uncertain results. Main-frame input fields only.
          parameters:
            top_origin:
              type: string
              description: Exact origin, e.g. https://example.com, without path or trailing slash.
              required: true
              max_length: 256
            tab_id:
              type: string
              description: Optional backend tab ID to disambiguate several tabs on the same origin.
              max_length: 256
            fields:
              type: json_array
              description: 'One to eight metadata objects, each {"field_name":"username","css":"#username"} or {"field_name":"password","css":"#password"}; to deliver material the run already holds, add "value" set to its placeholder exactly, e.g. {"field_name":"code","css":"#otp","value":"[REF:otp]"}; for a code entered one digit per box, list one field per box in order, each with the same placeholder. Never include a typed value. Use unique CSS selectors, not snapshot @refs.'
              required: true
              max_json_bytes: 8192
              max_depth: 4
              max_nodes: 64
              max_items: 8
            connection_mode:
              type: string
              description: This trusted fill currently supports cdp only.
              default: cdp
              max_length: 16
          mappings:
          - type: runtime_control
            parameter: top_origin
          - type: runtime_control
            parameter: tab_id
          - type: runtime_control
            parameter: fields
          - type: runtime_control
            parameter: connection_mode
        help:
          description: Read agent-browser CLI documentation without opening or connecting to a browser.
          parameters:
            args:
              type: string_array
              description: 'Arguments after `agent-browser --help` or for command help. Examples: ["open","--help"],
                ["batch","--help"], ["skills","get","core","--full"].'
              max_items: 16
              max_item_bytes: 4096
            command:
              type: string
              description: Convenience single command token for '<command> --help'. Use args for multi-token
                docs commands. 'core' expands to 'skills get core --full'.
              max_length: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: command
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        read:
          description: Read agent-oriented page text. With a URL, uses the upstream read path; without
            one, reads the rendered active-tab DOM. Prefer this over screenshots for public text extraction.
            The result is a content document envelope — `document.text` (full), `excerpt` (bounded),
            `fetch_status`, `evidence_role`, `claim_eligible` — so a genuinely opened, complete page is
            claim-eligible evidence under the same rules as `content_read`; a degraded shell (login wall,
            error page, JavaScript-required page) is returned as `discovery_only` and supports no claim.
          parameters:
            args:
              type: string_array
              description: 'Common forms: [] for the active tab, ["https://example.com/article"], or include
                read flags from help({"args":["read","--help"]}).'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        a11y:
          description: Run the upstream accessibility audit and return structured findings. This is a
            diagnostic audit, distinct from snapshot refs used for interaction.
          parameters:
            args:
              type: string_array
              description: Usually []. Inspect help({"args":["a11y","--help"]}) before passing audit options.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        session:
          description: Inspect upstream session lifecycle state, including list, id, and info subcommands.
            Reuse a generated id unchanged; the patched driver bounds its readable prefix against the
            effective namespaced Unix socket path while preserving the stable hash suffix. The runtime
            preserves its explicit lifecycle by default and sets AGENT_BROWSER_IDLE_TIMEOUT_MS=0 unless
            an operator or engine resolver overrides it.
          parameters:
            args:
              type: string_array
              description: 'Common forms: [], ["list"], ["id"], ["id","--scope","worktree","--prefix","myapp"],
                ["info"]. Treat generated ids as opaque. Use help({"args":["session","--help"]}) for details.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        batch:
          description: Execute multiple agent-browser commands sequentially through the CLI native batch
            command.
          parameters:
            args:
              type: string_array
              description: Batch options only when `commands` is set, usually ["--bail","--json"] or ["--json"].
                Legacy string mode can pass full command strings after options.
              max_items: 16
              max_item_bytes: 4096
            commands:
              type: json_array
              description: Preferred structured subcommands. Each inner array is one exact command argv
                without `agent-browser`, e.g. [["fill","@e1","text"],["press","Enter"],["wait","--load","networkidle"]].
              required: false
              max_json_bytes: 65536
              max_depth: 12
              max_nodes: 2048
              max_items: 64
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: commands
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        auth:
          description: Manage agent-browser's encrypted credential vault. Save, list, show, login-with,
            or delete a named credential profile (URL + username + password). Subcommands route through
            args[0]. Use `--password-stdin` with `stdin` for passwords so real secret text never appears
            in argv, prompts, traces, or tool summaries. `auth` stdin is the only CLI stdin that resolves a
            `[REDACTED:<id>]` / `[REF:<id>]` placeholder, at final dispatch, and only for a password that is
            not bound to a site — a one-time code, or a password the user gave for a specific origin, is
            delivered with `secure_prompt_fill` instead. Command output is redacted before it returns to the LLM.
            `auth login <name>` waits for the saved URL's form, then fills and submits.
            `--no-navigate` uses the page already open and checks its origin first.
          parameters:
            args:
              type: string_array
              description: 'Subcommand-first: ["save","<name>","--url","https://example.com/login","--username","alice","--password-stdin"]
                with stdin set to a secret placeholder such as "[REDACTED:password]". Other forms: ["list"],
                ["show","<name>"], ["login","<name>"], ["login","<name>","--no-navigate"], ["delete","<name>"]. Avoid --password with literal
                secret argv. Use help({"args":["auth","--help"]}) for full syntax.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
          stdin: optional
        back:
          description: Navigate the active page back in history.
          parameters:
            args:
              type: string_array
              description: 'Usually no args. Example: []. Use help({"args":["back","--help"]}) for flags.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        check:
          description: Check a checkbox or check browser/tool state, depending on CLI subcommand syntax.
          parameters:
            args:
              type: string_array
              description: For checkbox work, usually ["<selector-or-ref>"]. Use help({"args":["check","--help"]})
                for exact syntax.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        click:
          description: Click an element by accessibility ref or selector.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["@e2"], ["button:has-text(\"Save\")"], or ["@e2","--new-tab"]. Prefer
                snapshot -i refs before clicking.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        clipboard:
          description: Read or write the system clipboard through agent-browser.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["read"], ["write","text"]. Use help({"args":["clipboard","--help"]})
                for exact syntax.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        confirm:
          description: Confirm an active browser dialog when supported by the CLI.
          parameters:
            args:
              type: string_array
              description: Usually no args or dialog-specific args. Use help({"args":["confirm","--help"]})
                for exact syntax.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        console:
          description: Read or clear page console messages.
          parameters:
            args:
              type: string_array
              description: 'Common forms: [], ["clear"]. Use help({"args":["console","--help"]}) for filters
                and output flags.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        cookies:
          description: Get, set, or clear browser cookies.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["list"], ["clear"], or set/get forms from help({"args":["cookies","--help"]}).'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        deny:
          description: Deny an active browser permission or dialog when supported by the CLI.
          parameters:
            args:
              type: string_array
              description: Usually no args or dialog-specific args. Use help({"args":["deny","--help"]})
                for exact syntax.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        dialog:
          description: Inspect, accept, dismiss, or respond to browser dialogs.
          parameters:
            args:
              type: string_array
              description: Common forms depend on CLI version. Use help({"args":["dialog","--help"]})
                for accept/dismiss/prompt syntax.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        diff:
          description: Inspect visual or DOM diffs when supported by the CLI.
          parameters:
            args:
              type: string_array
              description: Exact diff argv tokens. Use help({"args":["diff","--help"]}) before using.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        dblclick:
          description: Double-click an element by accessibility ref or selector.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["@e2"] or ["<selector>"]. Prefer snapshot -i refs before dblclick.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        download:
          description: Trigger or manage downloads through the browser.
          parameters:
            args:
              type: string_array
              description: 'Common form: ["<selector-or-ref>"] for a download-triggering element. Use
                help({"args":["download","--help"]}).'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        drag:
          description: Drag from a source element to a target element when DOM-level drag works.
          parameters:
            args:
              type: string_array
              description: 'Common form: ["<source-selector-or-ref>","<target-selector-or-ref>"]. Prefer
                this over raw mouse when DOM-level drag applies; if it does not change page-owned state,
                fall back to measured mouse coordinates.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        errors:
          description: Read or clear collected page errors.
          parameters:
            args:
              type: string_array
              description: 'Common forms: [], ["clear"]. Use help({"args":["errors","--help"]}) for filters.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        eval:
          description: Evaluate JavaScript in the page context. First-class tool for geometry, value reads,
            shadow/iframe reach-through, JSON-serializable computation, and page-state mutations.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["(() => document.title)()"]. Return JSON-serializable values. Use
                freely for geometry/value reads, shadow/iframe reach-through, computation, or page-state
                mutations; verify state-changing eval sequences at the meaningful boundary with page-owned
                evidence.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        fill:
          description: Clear an input-like element and set its value.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["<selector-or-ref>","<text>"]. For incremental keystrokes use type
                instead.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        find:
          description: Find elements by semantic query or positional helper.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["role","button","Save"], ["text","Checkout"], ["label","Email"],
                ["first","button"], ["nth","button","2"].'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        focus:
          description: Move keyboard focus to an element.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["<selector-or-ref>"]. Often followed by press or type.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        forward:
          description: Navigate the active page forward in history.
          parameters:
            args:
              type: string_array
              description: 'Usually no args. Example: []. Use help({"args":["forward","--help"]}) for
                flags.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        frame:
          description: Switch or inspect iframe context. For cross-origin iframes, switch frames before
            using DOM/eval tools; parent eval cannot reach through cross-origin contentDocument.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["<iframe-selector>"], ["main"], or forms from help. Same-origin
                iframes may also be reached from parent eval via contentDocument. Cross-origin iframes
                require frame switching for DOM/eval; visible mouse/keyboard fallback can work only with
                screenshot/visible verification.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        get:
          description: Read page or element data such as text, html, value, attr, title, url, count, box,
            or styles.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["text","body"], ["html","body"], ["value","@e1"], ["attr","@e1","aria-label"],
                ["box","@e1"], ["title"], ["url"], ["count","button"].'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        highlight:
          description: Highlight elements in the browser for visual debugging.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["<selector-or-ref>"] plus any flags from help({"args":["highlight","--help"]}).'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        hover:
          description: Hover the pointer over an element.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["<selector-or-ref>"]. Verify hover menus with snapshot or screenshot.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        inspect:
          description: Inspect browser/page internals through CLI helpers.
          parameters:
            args:
              type: string_array
              description: Exact inspect argv tokens. Use help({"args":["inspect","--help"]}) before using.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        is:
          description: Check element state such as visible, enabled, checked, or selected.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["visible","<selector-or-ref>"], ["enabled","@e1"], ["checked","@e1"].
                Use help for supported predicates.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        keyboard:
          description: Issue lower-level keyboard events when press/type are not enough.
          parameters:
            args:
              type: string_array
              description: Exact keyboard argv tokens. Prefer press for normal keys and chords unless
                help says keyboard is needed.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        mouse:
          description: Issue low-level mouse events such as move, down, up, and wheel as a fallback when
            semantic primitives do not apply or do not change state.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["move","193","398"], ["down"], ["up"], ["wheel","500"], ["wheel","500","0"].
                Coordinates must be integer CSS pixels; round values before calling.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        network:
          description: Inspect or control network routing, requests, and HAR capture.
          parameters:
            args:
              type: string_array
              description: '["requests"], ["requests","--filter","api"], ["route","https://example.com/*","--abort"],
                ["unroute"], ["har","start"], ["har","stop","/tmp/site.har"].'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        open:
          description: Navigate the active page to a URL.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["https://example.com"] or ["http://localhost:5173/path"]. Wait or
                snapshot after navigation when page load matters.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        pdf:
          description: Render the active page to a PDF file.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["<path>"] plus any print flags from help({"args":["pdf","--help"]}).'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        press:
          description: Press a keyboard key or chord.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["Enter"], ["Control+a"], ["Meta+k"]. Focus an element first when the
                key is element-specific.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        record:
          description: Record the open page to .webm or .mp4.
          parameters:
            args:
              type: string_array
              description: '["start","./demo.webm"], ["start","./demo.webm","--cursor","--contact-sheet"],
                ["start","./demo.webm","--fps","30"], ["stop"]. --contact-sheet saves a PNG per visual change.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        reload:
          description: Reload the active page.
          parameters:
            args:
              type: string_array
              description: Usually no args. Use wait or snapshot after reload when page state matters.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        screenshot:
          description: Capture a screenshot artifact of the page or an element.
          parameters:
            args:
              type: string_array
              description: '[], ["--full"], ["--annotate"], ["--if-changed"], ["--if-changed","--threshold","0.01"].
                --if-changed skips an image the page did not change.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        scroll:
          description: Scroll the page or an element.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["down"], ["up"], ["down","800"], or selector-specific forms
                from help({"args":["scroll","--help"]}).'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        scrollintoview:
          description: Scroll an element into view.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["<selector-or-ref>"]. Useful before click, hover, or get box.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        select:
          description: Select option values in a dropdown/listbox.
          parameters:
            args:
              type: string_array
              description: 'Common form: ["<selector-or-ref>","<value-or-label>"]. Use help({"args":["select","--help"]})
                for multi-select syntax.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        set:
          description: Set browser/page options such as viewport, device, geolocation, network, headers,
            or media.
          parameters:
            args:
              type: string_array
              description: Common forms depend on CLI version. Use help({"args":["set","--help"]}) for
                viewport/device/geo/offline/header syntax.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        skills:
          description: Inspect agent-browser skills and built-in command guides.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["list"], ["get","core","--full"]. Use for version-matched browser
                automation guidance.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        snapshot:
          description: Capture the accessibility tree with @ref tags for reliable element targeting.
          parameters:
            args:
              type: string_array
              description: '["-i"], ["--compact"], ["--depth","3"], ["--selector","main"], ["-i","--delta"].
                --delta is a ref and line splice after the first full tree; add --full to reset. Refs last
                for the same document. Snapshot again after navigation.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        state:
          description: Inspect browser/page/session state through CLI helpers.
          parameters:
            args:
              type: string_array
              description: Exact state argv tokens. Use help({"args":["state","--help"]}) before using.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        storage:
          description: Read or modify localStorage/sessionStorage.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["local","list"], ["session","get","key"], ["local","set","key","value"].
                Use help for exact syntax.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        stream:
          description: Use agent-browser streaming helpers when available.
          parameters:
            args:
              type: string_array
              description: Exact stream argv tokens. Use help({"args":["stream","--help"]}) before using.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        tab:
          description: 'Manage tabs: list, create, switch, and close.'
          parameters:
            args:
              type: string_array
              description: '["list"], ["new"], ["new","--label","docs","https://example.com"], ["t1"],
                ["docs"], ["close","t1"]. Ids look like t1 and are not reused. A label works as an id.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        trace:
          description: Start, stop, or inspect browser traces when supported by the CLI.
          parameters:
            args:
              type: string_array
              description: Exact trace argv tokens. Use help({"args":["trace","--help"]}) before using.
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        type:
          description: Type text into the focused element or a target element using keystrokes.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["<selector-or-ref>","text"] or ["text"] after focus. Use fill
                when replacement is intended.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        uncheck:
          description: Uncheck a checkbox.
          parameters:
            args:
              type: string_array
              description: 'Usage: ["<selector-or-ref>"]. Verify with is checked or snapshot when needed.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        upload:
          description: Upload one or more files through a file input. Works when the input element is
            reachable via selector/ref. For uploads where the file input is hidden behind a button click
            (Google-Forms-style modal-iframe pickers, custom widgets that delegate to the OS dialog),
            use awaitfilechooser instead.
          parameters:
            args:
              type: string_array
              description: 'Common form: ["<selector-or-ref>","/absolute/file/path"]. Use help({"args":["upload","--help"]})
                for multiple files.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        setinterceptfilechooser:
          description: 'Toggle CDP file-chooser interception (Page.setInterceptFileChooserDialog). When
            on, native OS file pickers are suppressed and Page.fileChooserOpened fires instead. Required
            when the page (or an iframe) triggers an upload via a button-click chain that ends in input.click()
            on a hidden <input type=file>. Combined with awaitfilechooser to programmatically supply the
            file. Runtime fork: also enables interception on every attached iframe session, so file pickers
            fired from cross-origin iframes are caught too.'
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["on"], ["off"]. Also accepts ["true"], ["false"], ["enable"],
                ["disable"], ["1"], ["0"], or no args (defaults to on).'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        awaitfilechooser:
          description: Wait for the next Page.fileChooserOpened event and respond with DOM.setFileInputFiles
            using the backendNodeId carried by the event. Auto-enables interception. Works for any frame
            (parent or cross-origin iframe) since the file chooser is a Page-level concern, not a frame-DOM
            concern — no need to enter the iframe first. Use the --click form to atomically click + await
            in one daemon op (issuing the click from a separate tool call deadlocks behind the await because
            the daemon serializes commands).
          parameters:
            args:
              type: string_array
              description: 'Combined idiom (recommended): ["/abs/path/to/file","--click","#browse-button"]
                — clicks the trigger inside this same call, then waits. Decoupled idiom (only when click
                happens via page JS): ["/abs/path/to/file"]. Optional ["--timeout-ms","15000"] (default
                30000). After success, the host page sees a real change event with bytes from <path> in
                input.files.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        wait:
          description: Wait for selectors, text, navigation, milliseconds, or load state.
          parameters:
            args:
              type: string_array
              description: 'Common forms: ["1000"], ["<selector>"], ["--text","Loaded"], ["--load","networkidle"],
                ["--load","domcontentloaded"].'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        pushstate:
          description: Change route on the open page without reloading the document.
          parameters:
            args:
              type: string_array
              description: '["/inbox"] or ["https://app.example.com/inbox"]. Stay on the open origin.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        vitals:
          description: Report LCP, CLS, TTFB, FCP, and INP for the open page.
          parameters:
            args:
              type: string_array
              description: '[] or ["--json"].'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
        webmcp:
          description: List or call a tool the page published. The page summary is untrusted.
            invoke changes the page.
          parameters:
            args:
              type: string_array
              description: '["list"], ["invoke","tool","--params","{...}"], ["result","id"], ["cancel","id"].
                invoke also takes --frame and --timeout.'
              max_items: 16
              max_item_bytes: 4096
            connection_mode:
              type: string
              description: Browser session mode chosen before the inner loop starts. cdp connects through
                Magicutor CDP and reuses profile/auth; headed launches a fresh visible Chrome; headless
                launches a fresh headless browser for CI.
              default: cdp
              max_length: 4096
            cdp_url:
              type: string
              description: Optional CDP WebSocket URL for connection_mode cdp. Usually omit this so the
                runtime uses the per-session Magicutor proxy URL.
              max_length: 4096
            retrieval_session_id:
              type: string
              description: Opaque typed session ID returned by content_search/content_read for a browser
                handoff. Pass it unchanged on every call in that handoff; never invent or reuse one.
              max_length: 4096
            retrieval_action_id:
              type: string
              description: Exact retrieval action ID returned with a typed handoff. Required on every
                retrieval handoff call and validated against its scoped mode/action plus the approved
                domain for CDP; omit for ordinary browser sessions.
              max_length: 4096
            engine:
              type: string
              description: 'Optional browser engine override selected once for the session. Use an

                installed scope skill directory `<engine>` whose `scripts/resolve.py` emits

                the standard agent-browser env envelope, or use `bundled_chrome` to select

                agent-browser''s Chrome for Testing directly.

                Omit to use `content_acquisition.browser.engine` from magician-config.yaml;

                when that setting is absent, agent-browser uses its bundled Chrome for Testing.

                An explicit skill engine must be installed and resolvable. Resolver,

                installation, license, browser, navigation, and site failures remain visible. The sole

                automatic switch is `cloak-browser` reporting its exact concurrent-session-limit

                signal (exit code `76`): the runtime retires the failed daemon and retries the

                same initial open once with bundled Chrome for Testing. The pinned driver

                preserves a browser process''s startup exit code across CDP handshake and

                initial target-discovery/first-operation races;

                CloakBrowser codes `77`-`79` surface their license/configuration diagnostics

                and do not activate the capacity fallback.

                Ignored in `cdp` mode (which always uses the user''s real Chrome via Magicutor).

                `lightpanda` is strictly headless and is the first browser-engine preference

                for high-volume/fan-out reading and fresh public, profile-free work grounded

                in DOM/text, accessibility snapshots, semantic navigation, and structured

                extraction. Static/API/RSS retrieval should still precede any browser. High

                volume is not permission to bypass per-origin robots/rate/concurrency limits.

                It is driven through agent-browser and never starts Lightpanda''s own LLM. Do not

                choose it when the task may require screenshots, PDFs, visual or coordinate

                evidence, headed presentation, profiles, extensions, authenticated state,

                file workflows, or anti-bot Chromium fidelity; keep the configured

                full-fidelity engine instead. CloakBrowser supports both headless and headed

                operation. These are capability boundaries, not a site allowlist.

                Screenshot/PDF/record commands are rejected on an active Lightpanda session.

                On a compatible Lightpanda failure, close it and create a fresh headless

                session with the configured full-fidelity engine; if that engine is absent,

                `bundled_chrome` explicitly selects Chrome for Testing. Only deterministic

                public `content_read` automatically replays across this engine chain.

                '
              max_length: 4096
            url:
              type: string
              description: Optional starting URL hint. When set, the inner loop navigates here as its
                first step; it may navigate elsewhere afterward if the seed objective requires it. Pass
                when the user's request names a specific page (e.g., 'open https://example.com/login'
                or 'check the IPL points table on cricbuzz.com'). Leave empty when the goal is open-ended
                ('research X' / 'find me Y') and let the inner loop pick the entry point.
              max_length: 4096
          mappings:
          - type: runtime_control
            parameter: args
          - type: runtime_control
            parameter: connection_mode
          - type: runtime_control
            parameter: cdp_url
          - type: runtime_control
            parameter: retrieval_session_id
          - type: runtime_control
            parameter: retrieval_action_id
          - type: runtime_control
            parameter: engine
          - type: runtime_control
            parameter: url
          timeout_secs: 300
    runtime_catalog:
      categories:
      - browser
      composition_category: web_operations
      expose_timeout_control: false
---

# Browser

Tool name: `browser`
Use this for any web interaction.

Session-level parameters — pass them as top-level keys (sibling to `args`) on
your FIRST browser call, the one that creates the session (usually `open`). They
are read once at session creation and ignored on later calls (the session is
cached for the execution):
- `connection_mode`: optional. Use `cdp` to attach through Magicutor CDP and
  reuse the user's profile, auth, cookies, and extensions. Use `headed` when a
  fresh visible Chrome is useful for local tests or human observation. Use
  `headless` only for CI-style runs where visibility is not needed. If omitted,
  the runtime defaults to the env selection (`MAGICIAN_AGENT_BROWSER_MODE`) or
  `cdp`.
- `cdp_url`: optional override for `cdp`.
- `engine`: optional browser-engine selector for `headless` or `headed`. Use an
  installed engine-skill name, or `bundled_chrome` for agent-browser's Chrome
  for Testing. Omit it to use `content_acquisition.browser.engine`; when neither
  is set, the bundled browser is also the default. An explicitly selected
  skill engine must resolve successfully; resolver, installation, license,
  browser, navigation, and site failures remain visible. The sole automatic
  switch is `cloak-browser` reporting its exact concurrent-session-limit signal
  (exit code `76`): the runtime retires the failed daemon and retries the same
  initial open once with bundled Chrome for Testing. The pinned browser driver
  preserves licensed-browser startup exit codes even when the process publishes
  its CDP endpoint immediately before exiting or closes during initial target
  discovery/the first CDP operation; CloakBrowser codes `77`–`79`
  therefore report license/configuration diagnostics instead of a misleading
  WebSocket-handshake error. CDP ignores `engine`
  because it connects to the user's existing browser through Magicutor.
  `lightpanda` is strictly headless and never starts its own LLM: the runtime
  drives it through agent-browser's CDP engine. Prefer it as the first browser
  engine for high-volume/fan-out reading and fresh public, profile-free DOM/text,
  accessibility-snapshot, semantic-navigation, and structured-extraction
  sessions. Static/API/RSS retrieval should still precede any browser, and high
  volume must retain per-origin robots/rate/concurrency limits. Keep the
  configured full-fidelity engine when screenshots, PDFs,
  visual/coordinate evidence, headed presentation, profiles, extensions,
  authenticated state, file workflows, or anti-bot Chromium fidelity may be
  needed. These are capability-based soft boundaries, not a site allowlist.
  Lightpanda cannot emit real rendered screenshot/PDF/recording evidence; the
  runtime rejects those commands instead of returning a misleading placeholder.
  If a compatible Lightpanda session fails, close it and retry from a fresh
  headless session with the configured full-fidelity engine (normally
  `cloak-browser`, which supports both headless and headed operation). If that
  engine is unavailable, use explicit `engine: bundled_chrome` for Chrome for
  Testing. Automatic multi-engine replay remains limited to deterministic public
  `content_read`; never replay authenticated or side-effecting actions blindly.
- `retrieval_session_id`: only for a typed handoff returned by `content_search`
  or `content_read`. Pass the opaque ID unchanged on every handoff call. Do not
  invent, edit, or reuse it for another task.
- `retrieval_action_id`: required on every retrieval handoff call. Pass the
  exact action ID from the handoff so the runtime can enforce its scoped mode,
  action, and (for CDP) domain-bound approval. Omit it for ordinary browser work.

Retrieval handoffs ignore generic browser mode environment overrides. Start the
handoff with the exact `connection_mode` and target URL from the typed result.
Authenticated handoffs are rejected when their approval expires, the scope or
action differs, or the tab leaves the approved domain.

Example (mode chosen on the session-creating call):
`open({"args":["https://example.com"],"connection_mode":"headed"})`.

Example (fast public DOM-first session):
`open({"args":["https://example.com"],"connection_mode":"headless","engine":"lightpanda"})`.

The browser tools mirror the pinned `agent-browser` CLI one-to-one — each tool is
a CLI command that takes exact `args` tokens after the command name — rather than
a parallel backend browser API.

Use `help` first when command details matter:
- `help({})` -> `agent-browser --help`
- `help({"args":["skills","get","core","--full"]})`
- `help({"args":["click","--help"]})`
- `help({"args":["batch","--help"]})`

Command-level help:
- Most agent-browser commands support `--help`.
- Prefer `help({"args":["<command>","--help"]})` for command docs because
  it does not open/connect to a browser.

The pinned driver includes upstream `read`, accessibility
audit, session/restore, WebGPU, renderer recovery, and stricter allowed-domain
support. Use `read` for agent-oriented public text or rendered active-tab text;
use `snapshot` for interactive refs; use `a11y` only when an accessibility audit
is actually required. `session list|id|info` is available for diagnostics. Treat
the value returned by `session id` as opaque and reuse it unchanged; on Unix the
driver shortens only its readable prefix when necessary to fit the effective
namespaced socket path while preserving the stable hash suffix.
The runtime owns browser lifecycle and preserves its previous no-idle-shutdown
behavior by setting `AGENT_BROWSER_IDLE_TIMEOUT_MS=0`; an explicit operator or
engine-resolver value can opt into upstream's timeout. Use `close
--keep-browser` only when the user wants a locally launched visible window
handed off after automation; ordinary `close` remains the cleanup path.
Successful close commands wait for their CLI daemon to finish cleanup before
returning, so an immediate reconnect starts with a fresh owner and stable refs.

Through the Chrome extension proxy, `download <ref> <path>` supports concrete
links up to 16 MiB. It fetches in the page's authenticated context, follows page
CORS rules, and writes the received bytes atomically on the backend machine
(including Linux). Scripted download buttons and larger files require another
download route. Ordinary browser CDP downloads keep their existing behavior.

Browser CLI commands are declared as direct native tools in this pack. Each
command accepts exact `args` tokens after the command name:
- `open({"args":["https://example.com"]})`
- `snapshot({"args":["-i"]})`
- `click({"args":["@e2","--new-tab"]})`
- `wait({"args":["--load","networkidle"]})`
- `screenshot({"args":["--full","/tmp/page.png"]})`

For a login that needs credentials which are not already saved/authorized,
call the built-in `secure_prompt_fill` action. It is a runtime action, not an
agent-browser CLI subcommand, and must not be placed in `batch.commands`:

`secure_prompt_fill({"top_origin":"https://example.com","fields":[{"field_name":"username","css":"#username"},{"field_name":"password","css":"#password"}]})`

Discover stable, unique CSS selectors before calling it; snapshot `@refs` are
not accepted. The runtime privately prompts for every field and asks for **Use
once** confirmation. The tool fills the bound document without enrolling the
values, invoking form submission, or returning credentials to you. Use the
configured local CDP session; unsupported transports/frames require a human
external-action handoff. Do not fall back to asking for credentials in chat,
generic forms, argv, or tool arguments.

When the run already holds the material — the user answered a
`need_user_input` password or `otp` ask and you were given a placeholder such
as `[REF:otp]` — deliver it with the same action by naming the placeholder as
the field's `value`:

`secure_prompt_fill({"top_origin":"https://example.com","fields":[{"field_name":"code","css":"#otp","value":"[REF:otp]"}]})`

The fill is the only place that placeholder becomes a value: `fill`, `type`,
`batch` commands and CLI arguments refuse it. A code entered one digit per
box is one call listing one field per box, in order, each with the same
placeholder — the runtime splits the code itself; you never see a digit. A
one-time code is reserved for this exact origin, spent as the fill starts,
and never filled twice; if the
receipt says `material_unavailable`, the code expired or was already used —
ask the user for a fresh one. A password the user gave for a specific origin
fills only on that origin (`material_unavailable`, `bound to …` elsewhere).
After a fill that delivered a secret, `screenshot`, `pdf` and `record` are
withheld until the page has demonstrably moved on: a navigation command
(`open`, `back`, `forward`, `reload`), or a `snapshot`/`text` of the page that
no longer shows the delivered value. Clicking submit does not clear it by
itself — after the page settles, take a `snapshot` and screenshots return.
Text snapshots are scrubbed. After a digit-per-box fill nothing that reads
the page runs (snapshots included) until you navigate: click submit, then
`reload` (or `open` the page you expect) before observing again.

After a `filled` receipt, use the already identified login/continue control
if submission is authorized. Verify the resulting authenticated page without
reading back credential fields or taking a screenshot of the filled login
form. Partial/uncertain receipts do not imply rollback: do not automatically
retry. A fresh attempt needs fresh input and approval.

Existing saved-profile authentication remains available through `auth`; use
only already authorized profiles or secret placeholders on that separate
path. Never place a literal credential in `args` or save one-time inputs.

Use the CLI's native `batch` command to execute multiple commands without
another LLM turn. Prefer structured `commands` over command strings so no
shell-style parsing, quoting, or decimal-coordinate splitting can break the
action. The dispatcher pipes `commands` to `agent-browser batch` JSON stdin.
Batch `commands` are agent-browser CLI subcommands only. Do not put the
runtime `help` tool inside `commands`; use the standalone `help` tool in a
separate turn when you need command documentation.
Common batch `args`:
- `--bail`: stop after the first failed subcommand.
- `--json`: return a JSON array of subcommand results.
Examples:
- `batch({"args":["--bail","--json"],"commands":[
    ["fill","@e1","user@example.com"],
    ["press","Enter"],
    ["wait","--load","networkidle"]]})`
- `batch({"args":["--json"],"commands":[
    ["open","https://example.com"],
    ["snapshot","-i"],
    ["screenshot","--full","/tmp/page.png"]]})`
Legacy batch string mode is still available with `args`, e.g.
`batch({"args":["--json","open https://example.com","snapshot -i"]})`.

`args` are argv tokens after the command. Do not include shell quotes,
pipes, `&&`, redirection, or the `agent-browser` binary name. If the CLI docs
show `agent-browser screenshot --full page.png`, call:
`screenshot({"args":["--full","page.png"]})`.

The Rust dispatcher does not translate combinations. If a CLI flag or
ordering is supported by the pinned `agent-browser`, pass those exact tokens.
If a combination is unsupported, let the CLI return the error and recover
using `help`, `snapshot`, or another exact command.

Prefer semantic/high-level primitives (`click`, `check`, `uncheck`, `select`,
`fill`, `press`, `drag`) over raw `mouse` coordinates when they apply to the
visible target. If a semantic primitive does not change state, do not repeat
the same primitive with the same target/arguments unless new evidence
suggests it should now work; consider raw mouse, eval geometry, frame/shadow
context, or another grounding source. Custom/canvas/coordinate-driven
widgets (draggable thumbs without ARIA, contenteditable surfaces,
hand-rolled sliders) may have weak or missing keyboard handlers; choose the
primitive that gives the cleanest evidence and state change for the current
page.

Verify effects yourself, from the page's own state. A browser command
succeeding means it ran — not that the page changed. After a mutating act that
should have changed something, read the page's own state back (a `get`/`is`
check, an attribute, a result value, or an `eval` that reads the DOM) to confirm
the effect actually landed. If an act ran but the target's state did NOT change,
do not repeat the same primitive with the same target/arguments — SWITCH
grounding (semantic primitive → eval geometry, refs → eval, main → frame/shadow,
or the native `drag <source> <target>`, which resolves element centres for you
and avoids stale coordinates). Use whatever method most reliably produces and
confirms the real effect; selector-bearing primitives are easy to re-read and
verify, but an `eval` that fires the element's own handlers is equally valid.

`mouse` / `drag` / raw coordinate actions return only a transport-level `✓ Done`
with NO page value — a stuck, missed, or no-op drag looks IDENTICAL to a
successful one. So after any drag, ALWAYS read the target's resulting state back
(its value / class / a `get` on the result element) rather than trusting the
`✓ Done`. This matters most for custom widgets (handle-less sliders, canvas,
contenteditable) whose handlers bind `mousedown` then listen for `mousemove`/
`mouseup` on the document: a discrete down→move→up via `mouse` can leave the
drag dangling (the up never reaching the document handler). If a drag does not
register, dispatch the move/up on the handle's OWNING document via `eval`
(`el.dispatchEvent(new MouseEvent('mousemove', {clientX, clientY, bubbles:true}))`),
which drives the page's own drag handlers reliably.

TARGET THE PRECISE DROP ELEMENT, NOT ITS CONTAINER. `drag` (and a coordinate
drop) lands on the target's CENTRE. A connector/handle/thumb drop zone is
usually a small element (e.g. an 18px `#connector-target`) nested inside a much
larger card/node wrapper (e.g. `#connector-target-node`). Dragging to the
wrapper drops on the wrapper's centre — usually empty space, NOT the hit zone —
so it silently fails to connect. Always resolve and target the innermost
specific drop element (the handle/target/thumb itself), not the section/card/
node that contains it. When a drag does not connect, the most common cause is
targeting the container; re-inspect for the inner drop element and retarget it.

Primitive success is transport/action success only. A successful browser
command means the command ran; it does not prove the page-level goal changed.
Verify at meaningful boundaries, not after every low-level primitive. A
meaningful boundary is the point where the page should have changed in a way
relevant to the goal: after a click that should open/submit/toggle
something, after a complete drag gesture, after a form-fill batch, after an
eval-and-event sequence, or after navigation/load. Use the smallest
page-owned evidence that proves progress — usually `get text
"<result-selector>"`, a single attribute, or a visible value — not a full
re-snapshot. Wide re-snapshots burn context.

Tracking requirements and finishing (the verification contract):
- Don't fake the grade. The manual `Pass`/`Fail` status radios (and any
  `data-status` they set) are the HUMAN grader's controls — do NOT `click`,
  `check`, `fill`, or `eval`-set them, and do NOT `eval`-click radios by index
  or `eval`-write a test's status. Toggling a Pass radio reports a result; it
  does not achieve one. To complete a case, perform its ACTUAL interaction so
  the PAGE registers it (scroll to and act on the specified element, drag the
  specified handle to its target, set the specified value, etc.) — by whatever
  method works: a semantic primitive, raw mouse, or an `eval` that fires the
  element's real handlers are all legitimate. A correctly-built page marks the
  case Pass ITSELF once the real interaction lands.
- Evidence is what the PAGE produced, never what you set. A value, result
  text, status element, or `is`/`get` check you read back FROM the page is
  evidence; a control you toggle (a Pass/Fail radio, a checkbox, a "done"
  button) or a value you `eval`-write is your own report, and proves nothing
  about whether the interaction worked. At each meaningful boundary, confirm
  the page's own indicator (the page-COMPUTED check items / result text, not a
  radio you set) shows the intended change before treating a step as done — and
  never record a step done off an action you have not verified actually changed
  the page.
- When the task has more than one requirement (every case / row / item /
  field), track them as durable task-state micro-goals: emit a
  `task_state_action` with one micro-goal per requirement, and mark a
  micro-goal `completed` only when you hold the page-owned evidence that it
  succeeded — put that evidence in the micro-goal's `evidence_refs`. If a
  requirement genuinely cannot be done, mark it `blocked` with the reason; do
  not silently skip it or complete it without evidence.
- Finishing is gated by the runtime. A `yield` that reports success while any
  requirement is still open (neither `completed` nor `blocked`) is rejected and
  handed back to you. Before yielding, complete each requirement with evidence
  or move the unfinished ones into the yield `open[]` / `blockers[]` fields — an
  honest partial (some completed, some open with blockers) always beats a
  blanket "all done" the page evidence does not support.

When a primitive succeeds but the observed state did not change, switch the
grounding source on the next turn (semantic → raw mouse, refs → eval
geometry, main frame → frame/shadow) unless new evidence says the same
approach should now work.

Frame context persists after `frame`. Return with `frame({"args":["main"]})`
before using main-page selectors, refs, screenshots, or text assumptions.

File uploads have two paths. Pick the right one for the target.
1. Direct — when the `<input type=file>` element is reachable via selector
   or @ref. Use `upload({"args":["@e3","/abs/path/file.txt"]})`. Bypasses
   the OS picker entirely via DOM.setFileInputFiles. This is the common
   case (95%+ of upload widgets).
2. Via picker — when the input is hidden and only triggered by a button
   click that calls `input.click()` (Google-Forms-style modal-iframe
   uploads, custom widgets that open the OS dialog). The hidden input is
   not selectable, so direct upload fails. Use the combined form:
   `awaitfilechooser({"args":["/abs/path/file.txt","--click","#browse-btn"]})`.
   This atomically clicks the trigger, intercepts the resulting file
   chooser, and supplies the file via the event's backendNodeId — no
   OS dialog ever shows. **Do not** issue the click from a separate tool
   call while awaitfilechooser is waiting; the daemon serializes commands
   behind a single mutex and the click will deadlock behind the await.
   Works for any frame (including cross-origin sandboxed iframes) since
   the file chooser is a Page-level CDP event, not a frame-DOM event.

JavaScript dialogs (alert / confirm / prompt): `alert` and `beforeunload`
are AUTO-ACCEPTED for you — they never block. So do NOT add a `dialog accept`
after a click that triggers an `alert` (e.g. a button whose handler calls
`alert(...)`): by the time it runs the dialog is already gone, and handling a
dialog that isn't open is a harmless no-op (the harness treats it as success,
not a failure). Only `confirm` and `prompt` need explicit handling — use
`dialog accept` / `dialog dismiss` (or `dialog accept "<text>"` for a prompt)
when the page's flow genuinely waits on one. When unsure, just proceed and read
the page's resulting state; the dialog won't be blocking you.

Eval is a first-class browser primitive. It is available for geometry,
value reads, shadow/iframe reach-through, structured computation, and
page-state mutations. It is not restricted to fallback use; choose it
whenever it is the most direct way to understand or change the page.

Eval may read or mutate page state (`.click()`, dispatching events, setting
`.value`, calling page functions). After an eval sequence intended to change
state, verify at the same meaningful boundary you would use for any other
action.

The accessibility tree only enumerates ARIA-exposed elements. Custom
widgets without role/aria-label, shadow-root descendants, and same-origin
iframe descendants may be easier to inspect with eval than with snapshot
refs. A compound `eval` that returns a JSON-serializable struct with the
coordinates or values you need this turn is usually efficient and avoids
repeated round trips.

Coordinate mouse ops require the target in the visible viewport FIRST. Raw
`mouse` and `drag` do NOT auto-scroll, and CDP mouse events at coordinates
outside the visible viewport silently fail to dispatch — the command reports
success but no event reaches the page (the most common cause of a drag that
"ran" yet moved nothing). Semantic primitives (`click`, `hover`) auto-scroll;
raw `mouse move`/`mouse down`/`mouse up`/`drag` and `getBoundingClientRect()` do
NOT. So before measuring or acting on viewport coordinates, scroll the target
into view first (`scrollintoview "<selector>"` for a known DOM element, or an
`eval` `Element.scrollIntoView({block:'center'})` for a node only reachable via
shadow/iframe traversal), then RE-MEASURE all rects — viewport coordinates
change after any scroll.

Custom-widget / dual-range-slider drag playbook (use when it fits the
control):
1. Scroll the case into view FIRST with `scrollintoview "#case-N"` — a
   coordinate `mouse`/`drag` whose target is off-viewport silently no-ops, so
   this is a precondition, not an "if needed" step. Then measure in that same
   viewport state; viewport-relative coords change after scrolling, so always
   scroll, then measure, then act.
2. A compound eval can return `{sliderY, currentMinX, currentMaxX,
   targetMinX, targetMaxX}` for both thumbs in a single call. Compute
   target X inline as `r.left + (TARGET-RANGE_MIN)/(RANGE_MAX-RANGE_MIN)*r.width`.
3. Round all coordinates to integer CSS pixels in your `mouse` JSON args.
4. Drag min thumb: `mouse move currentMinX sliderY` → `mouse down` →
   `mouse move targetMinX sliderY` → `mouse up`. Same for max thumb. One
   intermediate move suffices when handlers read `clientX` on every
   `mousemove`; add 5–10 interpolated moves only if the first attempt
   under-shoots.
5. Verify with `get text "#case-N-result"` (or smallest sentinel). Look
   for the page's own SUCCESS-style string before recording the case
   complete.
6. Keyboard interaction is optional. If a keypress does not change
   `aria-valuenow`, switch to a more useful primitive such as eval-derived
   geometry plus mouse drag.

Reach-through templates for compound eval (single line, JSON arg is one
string). Use the agent-browser `eval` tool form
`eval({"args":["<arrow-IIFE>"]})`:
- Main DOM: arrow IIFE gets bounding rects of slider track + min thumb +
  max thumb via `document.getElementById(...)`, then returns the struct
  described above.
- Shadow DOM: replace `document.getElementById('SLIDER')` with
  `document.getElementById('HOST_ID').shadowRoot.getElementById('SLIDER')`,
  same for thumbs.
- Same-origin iframe (page-relative coords): grab
  `const f = document.getElementById('IFRAME_ID')`, then
  `const ifr = f.getBoundingClientRect()`,
  `const idoc = f.contentDocument`. Read iframe-internal rects via `idoc`,
  then add `ifr.left` / `ifr.top` to convert to page coords for use with
  `mouse`.
- Discover the ids first — do NOT guess them. A reach against a wrong
  `HOST_ID` / `IFRAME_ID` returns `null` (not an error), which is exactly how a
  run wastes turns guessing selectors. Enumerate shadow hosts with
  `[...document.querySelectorAll('*')].filter(e=>e.shadowRoot).map(e=>e.id||e.tagName)`
  and iframes with
  `[...document.querySelectorAll('iframe')].map(f=>({id:f.id,src:f.src,hasSrcdoc:!!f.getAttribute('srcdoc'),sandbox:f.getAttribute('sandbox')}))`,
  then reach with the real id. Only OPEN shadow roots are reachable; a closed
  `shadowRoot` reads as `null` and cannot be entered — if every candidate host
  is closed, report it as blocked rather than retrying.
- Mouse `args` must be integer CSS pixels; round in your call site.

Iframe origin handling:
- First identify where the target lives: main document, same-origin iframe,
  cross-origin iframe, sandboxed iframe, or unknown. Pick the
  grounding/action path from that page shape.
- Same-origin iframe from parent context: parent `eval` may use
  `iframe.contentDocument` to read, mutate, or compute child geometry. Add
  the iframe element's parent-page rect offset when converting child rects
  to viewport mouse coordinates.
- Same-origin iframe via frame context: switching with `frame` is also
  valid. After switching, normal tools (`snapshot`, `get`, `click`, `fill`,
  `eval`, `mouse`, `screenshot`) operate inside the selected frame context.
- Cross-origin iframe from parent context: parent `eval` cannot access
  `contentDocument`, child DOM, child JS globals, or child selectors. Do not
  treat that failure as a page failure; it is the browser security boundary.
- Cross-origin iframe via frame context: use `frame` to select the iframe,
  then use normal tools inside that selected context. `eval` is still usable
  after a successful frame switch because it runs in the selected frame, not
  through parent `contentDocument`.
- Frame switching can silently FAIL for some sandboxes (e.g.
  `sandbox="allow-scripts"` without `allow-same-origin`): `frame <selector>`
  may report success without actually switching. After switching, CONFIRM with
  iframe-only state — `get title` returning the iframe's title, or an `eval`
  reading an id that exists only inside the iframe. If parent content comes
  back, the switch did not take; return to `frame main` and use the
  parent-coordinate path below.
- Cross-origin visible-coordinate fallback: if frame switching cannot expose
  the target but the iframe is visibly rendered, parent-context `mouse`,
  `keyboard`, `screenshot`, and viewport geometry can still interact with
  visible controls. Verify by visible page evidence or screenshot because
  parent DOM reads cannot prove child DOM state.
- `srcdoc` iframes are inspectable from the parent under ANY sandbox: read
  `iframe.getAttribute('srcdoc')` as a plain string to extract the target's
  offsets / dimensions / handler constants, then compute precise
  parent-viewport coordinates and drive the interaction with parent-context
  `mouse`/`keyboard` — OS-level synthesized events route into the iframe even
  when its DOM is script-blocked.
- Verify cross-origin / sandboxed iframe interactions through PARENT-observable
  signals: many fixtures `postMessage` from the iframe to the parent, which
  flips a parent-DOM status/result element or a visible summary count. Parent
  reads of the parent's own state are authoritative; parent reads of iframe DOM
  are blocked — so read what the parent page itself surfaces (e.g.
  `get text "#...-result"`), not iframe-internal text.
- Sandboxed or blocked iframe: if the frame blocks scripts, focus, form
  submission, or pointer events, try the available visible/keyboard path
  when evidence suggests it can work. Stop when evidence is inaccessible or
  repeated attempts stop making progress; call `cannot_proceed` with that
  boundary as the reason.
- Frame context persists. Return to `frame main` before using main-page
  selectors, refs, screenshots, or text assumptions.

Quick command reference. Use `help({"args":["<command>","--help"]})` for
exact version-matched syntax:
- `open <url>`
- `snapshot [-i] [--compact] [--depth <n>] [--selector <sel>] [--delta]`
- `click <sel> [--new-tab]`
- `fill <sel> <text>`, `type <sel> <text>`, `press <key>`
- `wait <sel|ms>`, `wait --text <text>`, `wait --load <state>`
- `screenshot [--full] [--annotate] [--if-changed] [path]`, `pdf <path>`
- `get text|html|value|box|styles <sel>`, `get attr <sel> <name>`, `get title|url|count`
- `find role|text|label|placeholder|testid|first|last|nth ...`
- `mouse move <x> <y>`, `mouse down`, `mouse up`, `mouse wheel <dy> [dx]`
- `tab list`, `tab new [--label name] [url]`, `tab t1|label`, `tab close [t1|label]`
- `pushstate <url>`, `webmcp list|invoke|result|cancel`, `vitals`
- `batch({"args":["--json"],"commands":[["get","text","body"],["snapshot","-i"]]})`
