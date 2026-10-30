#!/usr/bin/env python3
"""Convert a legacy router snapshot to model records and compatibility aliases.

Writes only the requested output files. The source/runtime is never changed
implicitly. Credentials remain references to environment variable names.
"""
from __future__ import annotations

import argparse
import copy
import hashlib
import json
import re
from collections import defaultdict
from pathlib import Path

import yaml

from magician_config_text import read_config_text


POLICIES = {
    "default": {"output_tokens": 32768, "timeout_secs": 600, "answer_reserve_tokens": 2048},
    "background": {"output_tokens": 8192, "timeout_secs": 300, "thinking": "low", "answer_reserve_tokens": 1024},
    "local": {"output_tokens": 4096, "timeout_secs": 300, "thinking": "none", "answer_reserve_tokens": 1024},
    "long_form": {"output_tokens": 65536, "timeout_secs": 600, "answer_reserve_tokens": 4096},
    "bounded": {"output_tokens": 16384, "timeout_secs": 300, "answer_reserve_tokens": 2048},
}


def materialize_migration_router(router: dict) -> dict:
    """Expand old IDs only to compare a one-time conversion's request contracts.

    Normal setup/evaluation readers must use the strict catalog reader. This
    migration-only adapter never writes its input or supplies runtime routing.
    """
    from llm_catalog_config import materialize_router

    source = copy.deepcopy(router)
    if source.get("catalog") is None:
        return source
    authored = source.pop("profiles", {})
    adaptive = source.pop("adaptive_profiles", {})
    aliases = dict(source["catalog"].pop("aliases", {}))
    result = materialize_router(source)
    profiles = result["profiles"]
    for name, profile in authored.items():
        if name in profiles or name in adaptive:
            raise ValueError(f"duplicate migration route {name}")
        profiles[name] = profile
    while aliases:
        before = len(aliases)
        for alias, target in list(aliases.items()):
            if alias in profiles or alias in adaptive:
                raise ValueError(f"duplicate model alias {alias}")
            if target in profiles:
                profiles[alias] = copy.deepcopy(profiles[target])
                del aliases[alias]
            elif target in adaptive and adaptive[target]["fast_profile"] in profiles:
                profiles[alias] = copy.deepcopy(profiles[adaptive[target]["fast_profile"]])
                del aliases[alias]
        if len(aliases) == before:
            raise ValueError("unresolved or cyclic model aliases")
    return result


def thinking_control(provider: str, model: str) -> dict:
    lower = model.lower()
    if provider == "openai":
        levels = ["low", "medium", "high", "xhigh", "max"]
        if lower not in {"gpt-6.1-sol", "gpt-6-astra"}:
            levels.insert(0, "none")
        return {"mode": "effort", "levels": levels, "default": "medium"}
    if provider == "anthropic":
        if "haiku-4-5" in lower:
            return {"mode": "budget", "levels": ["none", "low", "medium", "high"], "default": "none",
                    "budget": {"min_tokens": 1024, "max_tokens": 32000, "step_tokens": 1024}}
        levels = ["low", "medium", "high", "xhigh", "max"]
        if lower in {"claude-opus-4-7", "claude-opus-5"}:
            levels.insert(0, "none")
        return {"mode": "effort", "levels": levels, "default": "medium"}
    if provider == "gemini":
        levels = ["low", "medium", "high"]
        if "flash-lite" in lower:
            levels.insert(0, "minimal")
        return {"mode": "effort", "levels": levels, "default": "minimal" if "flash-lite" in lower else "medium"}
    if provider == "deepseek":
        return {"mode": "effort", "levels": ["none", "low", "high", "max"], "default": "low"}
    if provider == "minimax" and "m3" in lower:
        return {"mode": "toggle", "levels": ["none", "on"], "default": "on"}
    if provider == "ollama" and "embed" not in lower:
        return {"mode": "toggle", "levels": ["none", "on"], "default": "none"}
    if provider in {"sarvam", "xai"}:
        levels = (["none"] if provider == "sarvam" else []) + ["low", "medium", "high"] + (["xhigh"] if provider == "xai" else [])
        return {"mode": "effort", "levels": levels, "default": "low" if provider == "sarvam" else "medium"}
    return {"mode": "none", "levels": []}


def token_limits(provider: str, model: str, profiles: list[dict], root: dict) -> dict | None:
    """Published family limits, with physical local settings from the source.

    Account ceilings belong on connections. Unknown harness models deliberately
    have no declared limits. Source overrides narrower than a model's published
    window are retained in compatibility assignments below.
    """
    if provider.startswith("harness-"):
        return None
    if provider == "openai":
        context, output = 1050000, 128000
    elif provider == "anthropic":
        context, output = (200000, 64000) if "haiku-4-5" in model else (1000000, 128000)
    elif provider == "gemini":
        context, output = (1048576, 65536) if "gemini-4" in model else (1114112, 65536)
    elif provider == "deepseek":
        context, output = 1000000, 384000
    elif provider == "minimax":
        context, output = (1000000 if "M3" in model else 204800), 65536
    elif provider == "sarvam":
        context, output = 128000, 128000
    elif provider == "xai":
        context, output = 2000000, 128000
    elif provider == "yutori":
        # Existing configuration does not attest physical input capacity.
        return None
    elif provider == "ollama":
        ollama = root.get("runtime", {}).get("ollama", {})
        embedding = any(p.get("metadata", {}).get("embedding_profile") for p in profiles)
        context = ollama.get("embedding_context_tokens", 8192) if embedding else max(p.get("context_window_tokens", 32768) for p in profiles)
        output = 1 if embedding else min(16384, context - 2048)
    else:
        raise ValueError(f"No qualified catalog limits for provider {provider!r}")
    return {"context_window_tokens": context, "min_output_tokens": 16 if provider == "openai" else 1,
            "max_output_tokens": output, "shared_context_window": True,
            **({"max_input_tokens": 1048576} if provider == "gemini" and "gemini-4" not in model else {})}


def native_level(control: dict, profile: dict) -> str:
    if control["mode"] == "none":
        return "auto"
    metadata = profile.get("metadata", {})
    effort = profile.get("reasoning", {}).get("effort")
    if effort is None:
        effort = metadata.get("output_config", {}).get("effort")
    if effort is None:
        effort = "none"
    if control["mode"] == "toggle":
        return "none" if effort in {"none", "off"} else "on"
    if effort in control["levels"]:
        return effort
    if effort in {"none", "minimal"}:
        return control["levels"][0]
    return control["default"]


def model_label(provider: str, model: str) -> str:
    if provider == "openai":
        return model.replace("gpt-", "GPT-", 1).rsplit("-", 1)[0] + " " + model.rsplit("-", 1)[1].capitalize()
    if provider == "anthropic":
        parts = model.split("-")
        return "Claude " + parts[1].capitalize() + " " + ".".join(parts[2:])
    if provider == "gemini":
        return model.replace("gemini-", "Gemini ", 1).replace("-", " ").title().replace("Gemini", "Gemini")
    return model


def qualify_model_feedback(catalog: dict) -> None:
    """One model-level decorative hint; clients never infer tiers from names.

    Populate existing catalogs during migration and preserve owner overrides.
    Unknown models and opaque engine defaults stay standard (no burst).
    These are presentation categories, not live prices or measured latency.
    """
    for model in catalog.get("models", {}).values():
        name = model["model"].lower()
        if re.search(r"(^|[^a-z0-9])(instant|nano|mini|luna|haiku|flash|lite|fast)([^a-z0-9]|$)", name):
            feedback = "fast"
        elif re.search(r"(^|[^a-z0-9])(astra|opus|fable|pro|argon)([^a-z0-9]|$)", name) or name in {"minimax-m3", "grok-4.7"}:
            feedback = "premium"
        else:
            feedback = "standard"
        model.setdefault("feedback", feedback)


def qualify_off_controls(catalog: dict) -> None:
    """Upgrade previously qualified controls; retain deliberate owner subsets."""
    for model in catalog.get("models", {}).values():
        provider = catalog["connections"][model["connection"]]["provider"]
        name = model["model"]
        if (provider, name) not in {("anthropic", "claude-opus-4-7"),
                                    ("anthropic", "claude-opus-5"),
                                    ("sarvam", "sarvam-105b")}:
            continue
        qualified = thinking_control(provider, name)
        old_levels = [level for level in qualified["levels"] if level != "none"]
        control = model.get("thinking", {})
        if control.get("mode") == "effort" and control.get("levels") == old_levels:
            control["levels"] = qualified["levels"]


def qualify_assignment_fallbacks(catalog: dict) -> None:
    """Move legacy fallback metadata to canonical assignment references."""
    assignments = catalog.get("assignments", {})
    aliases = catalog.get("aliases", {})

    def target_for(name):
        seen = set()
        while name in aliases:
            if name in seen:
                raise ValueError(f"cyclic fallback alias {name}")
            seen.add(name)
            name = aliases[name]
        if name not in assignments:
            raise ValueError(f"fallback must reference an assignment: {name}")
        return name

    for assignment in assignments.values():
        metadata = assignment.get("metadata", {})
        legacy = metadata.pop("fallback_profile", None)
        current = assignment.get("fallback")
        if current is not None and legacy is not None and target_for(current) != target_for(legacy):
            raise ValueError("conflicting explicit and legacy assignment fallbacks")
        target = current if current is not None else legacy
        if target is not None:
            assignment["fallback"] = target_for(target)
    complete = set()
    for start in assignments:
        visiting = set()
        current = start
        while current not in complete:
            if current in visiting:
                raise ValueError(f"cyclic assignment fallback at {current}")
            visiting.add(current)
            current = assignments[current].get("fallback")
            if current is None:
                break
        complete.update(visiting)


def convert(root: dict) -> tuple[dict, dict]:
    router = root["llm"]["router"]
    if router.get("catalog", {}).get("models"):
        catalog = copy.deepcopy(router["catalog"])
        qualify_off_controls(catalog)
        qualify_coding_models(root, catalog)
        qualify_native_engine_models(catalog)
        qualify_model_feedback(catalog)
        qualify_assignment_fallbacks(catalog)
        return catalog, copy.deepcopy(router["operation_mapping"])
    profiles = router["profiles"]
    catalog = {"connections": {}, "models": {}, "policies": copy.deepcopy(POLICIES), "assignments": {}, "aliases": {}}
    connections: dict[tuple, str] = {}
    model_groups: dict[tuple, list[dict]] = defaultdict(list)
    for profile in profiles.values():
        connection = (profile["provider"], profile.get("api_key_env"), profile.get("api_base_url"),
                      profile.get("metadata", {}).get("openai_api_mode", "responses") if profile["provider"] == "openai" else None)
        if connection not in connections:
            base = connection[0]
            if base == "openai" and connection[3] == "chat":
                base += "-chat"
            elif base == "ollama":
                base += "-embedding" if profile.get("metadata", {}).get("embedding_profile") else "-generation"
            name = base
            suffix = 2
            while name in catalog["connections"]:
                name, suffix = f"{base}-{suffix}", suffix + 1
            record = {"provider": connection[0]}
            for key, value in zip(("api_key_env", "api_base_url"), connection[1:3]):
                if value is not None:
                    record[key] = value
            if connection[0] == "openai":
                record["metadata"] = {"openai_api_mode": connection[3]}
            catalog["connections"][name] = record
            connections[connection] = name
        model_groups[(connections[connection], profile["model"])].append(profile)

    model_ids = {}
    for connection_id, connection in catalog["connections"].items():
        if connection["provider"] == "sarvam":
            # Preserve the deployment's configured ceiling until its account
            # tier is qualified; the model's published capacity is larger.
            connection["max_output_tokens"] = max(p.get("max_output_tokens", 8192) for p in profiles.values() if p["provider"] == "sarvam")
    for (connection, model), group in model_groups.items():
        provider = catalog["connections"][connection]["provider"]
        model_id = connection + "/" + re.sub(r"[^A-Za-z0-9._-]+", "-", model).strip("-")
        if provider == "ollama" and not any(p.get("metadata", {}).get("embedding_profile") for p in group):
            model_id = connection + "/selected-generation"
        model_ids[(connection, model)] = model_id
        embedding = any(p.get("metadata", {}).get("embedding_profile") for p in group)
        metadata = {}
        for key in sorted({k for p in group for k in p.get("metadata", {})} - {"tool_choice", "fallback_profile", "thinking", "output_config", "openai_api_mode", "think"}):
            values = [p.get("metadata", {}).get(key) for p in group]
            if values[0] is not None and all(value == values[0] for value in values):
                metadata[key] = values[0]
        record = {"connection": connection, "model": model, "label": model_label(provider, model),
                  "thinking": thinking_control(provider, model),
                  "capabilities": {"vision": any(p.get("supports_vision") is True for p in group),
                                   "tools": any(p.get("supports_tool_calling") is True for p in group),
                                   "computer_use": any(p.get("supports_computer_use") is True for p in group),
                                   "streaming": any(p.get("metadata", {}).get("streaming") is True for p in group)},
                  "selectable": not (embedding or connection.endswith("-chat") or provider == "ollama"),
                  **({"embedding": True} if embedding else {})}
        if provider == "ollama" and not embedding:
            record["label"] = "Local model"
        limits = token_limits(provider, model, group, root)
        if limits:
            record["limits"] = limits
        if metadata:
            record["metadata"] = metadata
        catalog["models"][model_id] = record

    # Native local chat uses the same physical model/runner as generation, but
    # the tool-capable endpoint is a distinct transport. Compatibility choices
    # retain /api/generate through an explicit connection override.
    local_models_seen = set()
    for model in catalog["models"].values():
        connection = catalog["connections"][model["connection"]]
        if connection["provider"] == "ollama" and not model.get("embedding"):
            if model["model"] in local_models_seen:
                continue
            local_models_seen.add(model["model"])
            if connection["api_base_url"].endswith("/api/chat"):
                model["selectable"] = True
                continue
            chat_connection = model["connection"] + "-chat"
            chat = copy.deepcopy(connection)
            chat["api_base_url"] = re.sub(r"/api/(generate|chat)$", "/api/chat", connection["api_base_url"])
            catalog["connections"][chat_connection] = chat
            model["connection"] = chat_connection
            model["selectable"] = True

    canonical = {}
    for name, profile in profiles.items():
        candidates = [key for key in connections.values()
                      for connection in [catalog["connections"][key]]
                      if connection["provider"] == profile["provider"]
                      and connection.get("api_key_env") == profile.get("api_key_env")
                      and connection.get("api_base_url") == profile.get("api_base_url")
                      and (profile["provider"] != "openai" or connection.get("metadata", {}).get("openai_api_mode") == profile.get("metadata", {}).get("openai_api_mode", "responses"))]
        assert len(candidates) == 1, name
        model_id = model_ids[(candidates[0], profile["model"])]
        model = catalog["models"][model_id]
        assignment = {"model": model_id, "thinking": native_level(model["thinking"], profile)}
        # Retain an explicit Off-only admission restriction. Mandatory
        # reasoners normalize old impossible Off settings to their lowest
        # supported level, as they do in the native control migration.
        if profile.get("supports_reasoning") is False and assignment["thinking"] == "none":
            assignment["reasoning"] = False
        if candidates[0] != model["connection"]:
            assignment["connection"] = candidates[0]
        output = profile.get("max_output_tokens", 0)
        policy = "bounded" if name.startswith("relentless-text-") else "local" if profile["provider"] == "ollama" and not model.get("embedding") else "background" if output and output <= 8192 else "long_form" if output > 32768 else "default"
        if policy != "default":
            assignment["policy"] = policy
        context_limit = profile.get("context_window_tokens")
        if context_limit and context_limit < model.get("limits", {}).get("context_window_tokens", context_limit):
            policy = policy + "_context_" + str(context_limit)
            catalog["policies"].setdefault(policy, {**catalog["policies"][assignment.get("policy", "default")], "context_window_tokens": context_limit})
            assignment["policy"] = policy
        metadata = {key: value for key, value in profile.get("metadata", {}).items()
                    if key not in {"thinking", "output_config", "openai_api_mode", "think"}
                    and model.get("metadata", {}).get(key) != value}
        if metadata:
            assignment["metadata"] = metadata
        for old, new in (("chunking", "chunking"), ("default_modality", "modality"), ("temperature", "temperature")):
            if old in profile:
                assignment[new] = profile[old]
        for old, new in (("supports_vision", "vision"), ("supports_tool_calling", "tools"), ("supports_computer_use", "computer_use")):
            if old in profile and profile[old] != model["capabilities"][{"vision": "vision", "tools": "tools", "computer_use": "computer_use"}[new]]:
                assignment[new] = profile[old]
        signature = json.dumps(assignment, sort_keys=True)
        if signature not in canonical:
            route = "migration/" + hashlib.sha256(signature.encode()).hexdigest()[:12]
            canonical[signature] = route
            catalog["assignments"][route] = assignment
        catalog["aliases"][name] = canonical[signature]

    for name, adaptive in router.get("adaptive_profiles", {}).items():
        fast = profiles[adaptive["fast_profile"]]
        # Resolve through the already validated compatibility assignment.
        target = catalog["assignments"][catalog["aliases"][adaptive["fast_profile"]]]
        thinking = profiles[adaptive["thinking_profile"]]
        if fast["provider"] != thinking["provider"] or fast["model"] != thinking["model"]:
            raise ValueError(f"{name}: adaptive models differ; explicit migration is required")
        catalog["aliases"][name] = "model/" + target["model"] + "/auto"
    mappings = copy.deepcopy(router["operation_mapping"])
    # App trust declarations bind exact selectors, even when another alias
    # resolves to the same physical model. Keep those declared identities.
    trusted_profiles = root.get("app_platform", {}).get("processing", {}).get("profiles", {})
    renamed = {}
    for operation, selector in mappings.items():
        arms = [(None, selector)] if isinstance(selector, str) else [(key, selector[key]) for key in ("default", "when_cloud", "when_has_images") if key in selector]
        for arm, old in arms:
            if old in trusted_profiles:
                continue
            target = catalog["aliases"].get(old, old)
            if target in catalog["assignments"]:
                renamed.setdefault(target, "operation/" + operation + ("/" + arm if arm not in (None, "default") else ""))
                target = renamed[target]
            # Keep the submitted selector identity. App trust declarations,
            # sealed bounds and logical-context adapters bind this exact ID;
            # compatibility aliases provide the new settings behind it.
    catalog["assignments"] = {renamed.get(name, name): assignment for name, assignment in catalog["assignments"].items()}
    catalog["aliases"] = {name: renamed.get(target, target) for name, target in catalog["aliases"].items()}
    qualify_coding_models(root, catalog)
    qualify_native_engine_models(catalog)
    qualify_model_feedback(catalog)
    qualify_assignment_fallbacks(catalog)
    return catalog, mappings


def qualify_native_engine_models(catalog: dict) -> None:
    """Native coding controls, measured from the installed CLI transports.

    The one-shot provider controls stay independent. No token budgets are
    invented for subscription-backed engines.
    """
    models = catalog["models"]
    for name in ["grok-4.7", "grok-4.7-build-fast", "grok-4.6", "grok-4.5"]:
        key = f"harness-grok/{name}"
        if key not in models and "harness-grok/default" in models:
            model = copy.deepcopy(models["harness-grok/default"])
            model.update(model=name, label=name)
            models[key] = model
    for model in models.values():
        provider = catalog["connections"][model["connection"]]["provider"]
        levels = None
        if provider == "harness-claude_code" and model["model"] != "haiku":
            levels = ["low", "medium", "high"]
            if model["model"] in {"opus", "fable"}: levels += ["xhigh", "max"]
        elif provider in {"harness-codex", "harness-grok"}:
            levels = ["low", "medium", "high", "xhigh"]
        elif provider == "harness-agy":
            levels = ["low", "medium", "high", "xhigh", "max"]
        if levels:
            model.setdefault("coding_thinking", {"mode": "effort", "levels": levels, "default": "medium"})
    for key in ["harness-grok/grok-4", "harness-grok/grok-4-fast"]:
        if key in models: models[key]["selectable"] = False


def qualify_coding_models(root: dict, catalog: dict) -> dict[str, str]:
    """Retain the exact model pairs the install already qualified for Pi."""
    qualified = {}
    for profile in root.get("coding", {}).get("profiles", []):
        if not profile.get("enabled", True):
            continue
        target = profile["llm_profile"]
        seen = set()
        while target in catalog.get("aliases", {}):
            if target in seen:
                raise ValueError(f"cyclic coding selector {profile['id']}")
            seen.add(target)
            target = catalog["aliases"][target]
        assignment = catalog.get("assignments", {}).get(target)
        model_id = assignment.get("model") if assignment else None
        if model_id is None:
            model_id = next((key for key, model in catalog["models"].items()
                             if target in {f"model/{key}/{level}" for level in ["auto", *model.get("thinking", {}).get("levels", [])]}), None)
        if model_id is None:
            raise ValueError(f"coding selector {profile['id']} has no catalog model")
        catalog["models"][model_id]["coding"] = True
        qualified[profile["id"]] = model_id
    return qualified


def migrate_coding_default(text: str, root: dict, catalog: dict) -> str:
    default = root.get("coding", {}).get("default_profile")
    model = qualify_coding_models(root, catalog).get(default)
    if model is None:
        return text
    lines = text.splitlines(keepends=True)
    inside = False
    for index, line in enumerate(lines):
        if line.startswith("coding:"):
            inside = True
        elif inside and line.strip() and not line.startswith((" ", "#")):
            break
        elif inside and line.startswith("  default_profile:"):
            lines[index] = f"  default_profile: pi/model/{model}/auto\n"
            break
    return "".join(lines)


def render_tables(catalog: dict, mappings: dict) -> str:
    # Compact records keep compatibility aliases from dominating the authoring
    # surface. Catalog schemas are structured; users never edit generated routes.
    lines = ["# Provider connections, model capabilities, shared policies and routing.",
             "# Spliced under llm.router; legacy aliases preserve saved selections." if catalog.get("aliases")
             else "# Spliced under llm.router; selections use model/thinking and workload assignments.", "    catalog:"]
    for section in ("connections", "models", "policies", "assignments", "aliases"):
        if section == "aliases" and not catalog.get(section):
            continue
        if not catalog[section]:
            lines.append(f"      {section}: {{}}")
            continue
        lines.append(f"      {section}:")
        for key, value in catalog[section].items():
            if section == "models":
                lines.append(f"        {key}:")
                rendered = yaml.safe_dump(value, sort_keys=False, width=100)
                if catalog["connections"][value["connection"]]["provider"] == "ollama" and not value.get("embedding"):
                    rendered = re.sub(r"^model: .*?$", "model: *local_generation_model", rendered, flags=re.M)
                lines.extend("          " + line for line in rendered.splitlines())
                continue
            rendered = yaml.safe_dump(value, default_flow_style=True, sort_keys=False, width=10000).strip()
            if rendered.endswith("\n..."):
                rendered = rendered[:-4]
            lines.append(f"        {key}: {rendered}")
    mapping_text = yaml.safe_dump({"operation_mapping": mappings}, sort_keys=False, width=10000)
    lines.extend("    " + line for line in mapping_text.splitlines())
    return "\n".join(lines) + "\n"


def remove_legacy_router_tables(config_text: str) -> str:
    """Keep the main document and anchors; move only llm.router-owned tables.

    Older installs can have all router tables inline. Leaving those behind
    would make the loader ignore the newly migrated sibling catalog.
    """
    out = []
    in_llm = in_router = skipping = False
    for line in config_text.splitlines(keepends=True):
        content = line.strip()
        is_content = bool(content) and not content.startswith("#")
        indent = len(line) - len(line.lstrip())
        if is_content:
            if indent == 0:
                in_llm = content == "llm:"
                in_router = False
            elif in_llm and indent == 2:
                in_router = content == "router:"
            if skipping and indent <= 4:
                skipping = False
            if in_router and indent == 4 and content.split(":", 1)[0] in {
                "profiles", "operation_mapping", "adaptive_profiles", "catalog"
            }:
                skipping = True
        if not skipping:
            out.append(line)
    return "".join(out).replace("# Profiles live in `llm-router.yaml`", "# Model catalog and routing live in `llm-router.yaml`")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    if args.config.resolve().parent == args.output_dir.resolve():
        parser.error("output directory must differ from the source directory; review the migrated pair before installing it")
    root = yaml.safe_load(read_config_text(args.config, expand_catalog=False))
    catalog, mappings = convert(root)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "llm-router.yaml").write_text(render_tables(catalog, mappings))
    (args.output_dir / "magician-config.yaml").write_text(migrate_coding_default(remove_legacy_router_tables(args.config.read_text()), root, catalog))
    print(json.dumps({"connections": len(catalog["connections"]), "models": len(catalog["models"]),
                      "compatibility_assignments": len(catalog["assignments"]), "aliases": len(catalog["aliases"]),
                      "output": str(args.output_dir)}))


if __name__ == "__main__":
    main()
