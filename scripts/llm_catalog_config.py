"""Materialize catalog routes for config-reading setup/evaluation scripts.

The Rust resolver owns production dispatch. This reader generates its resolved
`router.profiles` view for scripts; parity is checked against the Rust catalog
inspector. Authored profiles, adaptive tables and aliases are accepted only by
the separate one-time migration tool. This reader never writes configuration.
"""
from __future__ import annotations

import copy
import struct


def selection_for_routing_handle(overview: dict, handle: str) -> dict[str, str]:
    """Resolve an assignment or generated handle before a typed client request.

    Model identity/effort comes from server metadata, never handle spelling or
    an arbitrary default. This function makes no provider or network calls.
    """
    selection = next((row.get("model_selection") for row in overview.get("profiles", [])
                      if row.get("name") == handle), None)
    models = overview.get("models", [])
    if selection is None:
        selection = next(({"model_id": model["id"], "thinking": level}
                          for model in models for level, route in model.get("choices", {}).items()
                          if route == handle), None)
    if not isinstance(selection, dict):
        raise ValueError(f"Model choice is unavailable: {handle}")
    model_id, thinking = selection.get("model_id"), selection.get("thinking")
    model = next((model for model in models if model.get("id") == model_id
                  and model.get("installed", True)), None)
    if not model or not isinstance(thinking, str) or thinking not in model.get("choices", {}):
        raise ValueError(f"Model choice is unavailable: {handle}")
    return {"model_id": model_id, "thinking": thinking}


def resolved_wire(wire: dict, thinking: dict) -> dict:
    """Mirror of Rust `ModelWireTraits::resolve`: request-shape facts for adapters."""
    mode = thinking.get("mode", "none")
    default_shape = {"none": None, "effort": "adaptive"}.get(mode, "budget")
    return {
        "thinking": None if mode == "none" else wire.get("thinking", default_shape),
        "thinking_off": "none" in thinking.get("levels", []),
        "thinking_off_effort": wire.get("thinking_off_effort"),
        "sampling": wire.get("sampling", True),
    }


def materialize_router(router: dict) -> dict:
    for retired in ("profiles", "adaptive_profiles"):
        if retired in router:
            raise ValueError(f"llm.router.{retired} is retired; migrate the configuration first")
    catalog = router.get("catalog")
    if catalog is None:
        return router
    if "aliases" in catalog:
        raise ValueError("catalog aliases are retired; use the one-time migration tool")
    result = copy.deepcopy(router)
    profiles = result["profiles"] = {}
    models = catalog.get("models", {})
    connections = catalog.get("connections", {})
    policies = catalog.get("policies", {})
    for records in (connections, models, catalog.get("assignments", {})):
        for record in records.values():
            metadata = record.get("metadata", {})
            if "fallback_profile" in metadata:
                raise ValueError("catalog fallbacks must use assignment.fallback, not metadata.fallback_profile")
            if "_magician_model_settings" in metadata:
                raise ValueError("reserved model settings metadata cannot be supplied by configuration")

    def resolve(assignment: dict) -> dict:
        model = models[assignment["model"]]
        connection = connections[assignment.get("connection", model["connection"])]
        base_connection = connections[model["connection"]]
        if connection["provider"] != base_connection["provider"]:
            raise ValueError("assignment cannot move a model to another provider")
        policy = {"output_tokens": 16384, "timeout_secs": 600, "answer_reserve_tokens": 2048,
                  **policies.get(assignment.get("policy", "default"), {})}
        if assignment.get("policy") and assignment["policy"] not in policies:
            raise ValueError("unknown model workload policy")
        limits = model.get("limits")
        thinking = model.get("thinking", {})
        mode = thinking.get("mode", "none")
        requested = assignment.get("thinking", "auto")
        level = policy.get("thinking", thinking.get("default", "none")) if requested == "auto" else requested
        if mode == "none":
            if requested != "auto" and level != "none":
                raise ValueError("model has no adjustable thinking")
            level = "none"
        elif level not in thinking.get("levels", []):
            if requested != "auto":
                raise ValueError("unsupported model thinking level")
            level = thinking["default"]
        hard_output = min((limits or {}).get("max_output_tokens", 2**32 - 1), connection.get("max_output_tokens", 2**32 - 1))
        output = min((limits or {}).get("default_output_tokens", policy["output_tokens"]), hard_output)
        metadata = {**copy.deepcopy(connection.get("metadata", {})), **copy.deepcopy(model.get("metadata", {})), **copy.deepcopy(assignment.get("metadata", {}))}
        locked_metadata = set(assignment.get("metadata", {}))
        if "fallback" in assignment:
            target = assignment["fallback"]
            if target not in catalog.get("assignments", {}):
                raise ValueError("unknown fallback assignment")
            if "fallback_profile" in metadata:
                raise ValueError("conflicting explicit and legacy assignment fallbacks")
            metadata["fallback_profile"] = target
            locked_metadata.add("fallback_profile")
        capabilities = model.get("capabilities", {})
        if model.get("selectable", True) and not model.get("embedding") and capabilities.get("tools"):
            metadata.setdefault("tool_choice", "auto")
        metadata["_magician_model_settings"] = {
            "model_id": assignment["model"],
            "selection": {"model_id": assignment["model"], "thinking": requested},
            "limits": limits,
            "thinking": {"mode": "none", "levels": [], "default": None, "budget": None, **thinking},
            "answer_reserve_tokens": policy["answer_reserve_tokens"],
            "max_output_tokens": hard_output,
            "locked_metadata": sorted(locked_metadata),
            "wire": resolved_wire(model.get("wire", {}), thinking),
        }
        profile = {"provider": connection["provider"], "model": model["model"],
                   "max_output_tokens": output, "timeout_secs": policy["timeout_secs"],
                   "supports_vision": assignment.get("vision", capabilities.get("vision", False)),
                   "supports_tool_calling": assignment.get("tools", capabilities.get("tools", False)),
                   "supports_computer_use": assignment.get("computer_use", capabilities.get("computer_use", False)),
                   "supports_reasoning": assignment.get("reasoning", mode != "none"), "metadata": metadata}
        for key in ("api_key_env", "api_base_url"):
            if key in connection:
                profile[key] = connection[key]
        for key in ("chunking", "temperature"):
            if key in assignment:
                profile[key] = assignment[key]
        if "chunking" in profile:
            profile["chunking"] = {"enabled": False, "safety_margin_tokens": 2048, "fallback_policy": "same_provider_only", **profile["chunking"]}
        if "temperature" in profile:
            profile["temperature"] = struct.unpack("f", struct.pack("f", profile["temperature"]))[0]
        if "modality" in assignment:
            profile["default_modality"] = assignment["modality"]
        if limits:
            profile["context_window_tokens"] = min(limits["context_window_tokens"], policy.get("context_window_tokens", 2**32 - 1))
        if mode != "none":
            reasoning = {"effort": level, "strategy": {"effort": "native_effort", "toggle": "toggle", "budget": "budget"}[mode]}
            if mode == "budget" and level != "none":
                budget = thinking["budget"]
                available = min(max(0, output - max(1, policy["answer_reserve_tokens"])), budget["max_tokens"])
                if available < budget["min_tokens"]:
                    raise ValueError("output allowance cannot fit thinking and answer")
                fraction = {"minimal": 8, "low": 8, "medium": 4, "high": 2}.get(level, 1)
                desired = max(available // fraction, budget["min_tokens"])
                steps = (desired - budget["min_tokens"]) // budget["step_tokens"]
                reasoning["max_reasoning_tokens"] = min(budget["min_tokens"] + steps * budget["step_tokens"], available)
            profile["reasoning"] = reasoning
        return profile

    def insert(name: str, assignment: dict) -> None:
        if name in profiles:
            raise ValueError(f"duplicate model route {name}")
        profiles[name] = resolve(assignment)

    complete = set()
    for start in catalog.get("assignments", {}):
        visiting = set()
        current = start
        while current not in complete:
            if current in visiting:
                raise ValueError("cyclic assignment fallback")
            visiting.add(current)
            current = catalog["assignments"][current].get("fallback")
            if current is None:
                break
            if current not in catalog["assignments"]:
                raise ValueError("unknown fallback assignment")
        complete.update(visiting)
    for model_id, model in models.items():
        for level in ["auto", *model.get("thinking", {}).get("levels", [])]:
            assignment = {"model": model_id, "thinking": level}
            if model.get("capabilities", {}).get("streaming"):
                assignment["metadata"] = {"streaming": True}
            insert(f"model/{model_id}/{level}", assignment)
    for name, assignment in catalog.get("assignments", {}).items():
        insert(name, assignment)
    result.pop("catalog", None)
    return result
