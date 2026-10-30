---
name: zomato-mcp
version: 0.1.1
description: |
  Use Zomato's official remote MCP for restaurant discovery, menus, carts and
  order inspection through browser OAuth with a loopback callback.
  Ordering/payment remain blocked pending a reviewed live
  checkout contract; never work around provider access restrictions.
metadata:
  magician:
    setup: { definition: governed-mcp-oauth }
    browser_signin:
      callback_mode: loopback
    skill_type: tool
    user_invocable: true
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: Live discovery requires a signed-in Zomato account; no non-interactive account fixture is supplied.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      runtime:
        protocol: mcp
        transport:
          kind: streamable_http
          endpoint: https://mcp-server.zomato.com/mcp
        discovery:
          namespace: zomato
          oauth:
            authorization_issuer: https://mcp-server.zomato.com/
            scopes: [mcp:tools, mcp:resources, mcp:prompts]
          commerce:
            commodity: INR
            resource_scope: commerce
            required_resource_authority: resource_authority
            amount_parameter: order_amount_inr
            checkout_name_terms: [order, checkout, payment, purchase, pay, confirm, reserve]
            cart_name_terms: [cart]
            cart_read_verb_terms: [get, view, fetch, show, summary, detail, current, list]
            total_field_priority: [finalamount, totalamount, billtotal, total, amount]
            cart_amount_unit: major
            allow_unverified_cart: false
        limits:
          timeout_secs: 90
          stdout_bytes: 33554432
          stderr_bytes: 1048576
      auth:
        kind: oauth_session
        requirement: required
        provider: zomato-mcp
        profile_selection:
          mode: selectable
          default: personal
      policy_floor:
        approval: ordinary
        resource_scopes: [external_mcp]
    runtime_catalog:
      categories: [commerce, shopping, food]
      composition_category: commerce_operations
      profile_parameter:
        name: profile
        enum_values: [personal]
---

# Zomato MCP

## Provider access

Use the shared Auth Broker's numeric loopback callback and begin normal OAuth.
The official manifest describes personal-use access requirements and a redirect
allowlist that omits loopbacks; do not treat that list alone as proof that a
local callback is rejected. Request vendor assistance if the actual flow returns
an explicit client/redirect rejection. Never borrow another application's
redirect URI or identity.

Use `status` to inspect the scoped account. On explicit sign-in, call
`auth_start` once. If sign-in cannot open, report the actual discovery or
registration error and its stage. Reaching the login page does not by itself
verify OTP completion, callback exchange or tool access.
Tokens stay in the encrypted principal/workspace vault; never request an OTP,
payment PIN or credential in chat.

## Use

After authorization, call `list_tools` and use only the returned names and
schemas with `call_tool` and `arguments_json`. No remote annotation grants
trust: unreviewed calls retain the runtime's conservative approval policy.
Confirm essential location, dietary preferences and item choices. Cart changes
require explicit buyer intent. Report actual tool results rather than assuming
an item was added or an order exists.

The initial package deliberately has no locally trusted commerce finalizers.
Order, checkout, confirmation and payment-like names fail closed at the generic
commerce boundary until live schemas and Resource Authority handling have been
reviewed. This conservative classifier may also block order-history/status
tools until exact read-only policies can be verified after sign-in.
Do not rename calls, change `risk`, use shell/HTTP or switch identities
to bypass that refusal. Offer the Zomato app for completing a purchase.

Official access requirements and endpoint:
https://github.com/Zomato/mcp-server-manifest
