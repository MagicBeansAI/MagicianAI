---
name: zepto-mcp
version: 0.2.0
description: |
  Toolskill for Zepto's official remote MCP server for Indian quick-commerce
  shopping. Use it to authenticate with browser OAuth, list Zepto's live MCP
  tools, call those tools through the MCP endpoint, search products, manage cart
  state, inspect order history, or place orders after runtime HITL approval.
  The hosted MCP endpoint is https://mcp.zepto.co.in/mcp and requires OAuth
  with an active Indian mobile number.
metadata:
  magician:
    setup: { definition: governed-mcp-oauth }
    skill_type: tool
    user_invocable: true
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          every action moves toward placing a real grocery order against the
          operator's account, and the package declares no read-only action.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      runtime:
        protocol: mcp
        transport:
          kind: streamable_http
          endpoint: https://mcp.zepto.co.in/mcp
        discovery:
          namespace: zepto
          oauth:
            authorization_issuer: https://auth.zepto.co.in
            scopes: [tools:read, tools:write]
          tool_policies:
            create_order:
              risk: commerce
              trusted_description: Create the reviewed order only when confirmOrder is true and product approval plus INR resource authorization have completed.
            create_online_payment_order:
              risk: commerce
              trusted_description: Create the reviewed online-payment order only after product approval and INR resource authorization.
            create_wallet_order:
              risk: commerce
              trusted_description: Create the reviewed wallet order only after product approval and INR resource authorization.
            create_upi_reserve_pay_order:
              risk: commerce
              trusted_description: Create the reviewed UPI reserve-pay order only after product approval and INR resource authorization.
          commerce:
            commodity: INR
            resource_scope: commerce
            required_resource_authority: resource_authority
            amount_parameter: order_amount_inr
            conditional_final_tools:
              create_order: {pointer: /confirmOrder, equals: true}
              create_online_payment_order: {pointer: /confirmOrder, equals: true}
              create_wallet_order: {pointer: /confirmOrder, equals: true}
              create_upi_reserve_pay_order: {pointer: /confirmOrder, equals: true}
            checkout_name_terms: [checkout, payment_link, reserve_pay, purchase]
            cart_name_terms: [cart]
            cart_read_verb_terms: [get, view, fetch, show, summary, detail, current, list]
            total_field_priority: [finalamount, totalamount, billtotal, total, amount]
            cart_amount_unit: major
            absolute_tolerance_minor: 2500
            percentage_tolerance_bps: 500
            allow_cartless_checkout: false
            allow_unverified_cart: false
        limits:
          timeout_secs: 90
          stdout_bytes: 33554432
          stderr_bytes: 1048576
      auth:
        kind: oauth_session
        requirement: required
        provider: zepto-mcp
        profile_selection:
          mode: selectable
          default: personal
      policy_floor:
        approval: ordinary
        resource_scopes: [external_mcp]
    runtime_catalog:
      categories: [commerce, shopping, groceries]
      composition_category: commerce_operations
      profile_parameter:
        name: profile
        enum_values: [personal]
---

# Zepto MCP

Zepto's MCP is a hosted remote MCP server for Zepto customers in India. The
governed runtime connects to `https://mcp.zepto.co.in/mcp` through the
official Rust MCP SDK. First use opens browser OAuth through Magican's scoped Auth
Broker; tokens remain in the encrypted principal/workspace vault.

It exposes live shopping workflows through the remote tools Zepto advertises:
product search, cart management, order placement, payment-method selection, and
order history. It is not a sandbox: checkout creates real Zepto orders for the
authenticated account.

Official sources:
- GitHub: https://github.com/zeptonow/mcp
- Endpoint: https://mcp.zepto.co.in/mcp

## Tool Actions

Use `auth_start` or `list_tools` first. If credentials are missing or expired,
the Auth Broker opens the user's browser, receives the fixed localhost callback,
and the SDK verifies access by listing tools. Never ask the user to share an
OTP, card detail, UPI PIN, or any other secret in chat.

- `status`: inspect the exact scoped OAuth binding without exposing a token.
- `auth_start`: start browser OAuth for the exact profile binding.
- `list_tools`: list live remote Zepto MCP tools and input schemas.
- `call_tool`: call a remote Zepto MCP tool by name with `arguments_json`.
- `clear_auth`: delete scoped OAuth state for the selected profile binding.

## Operating Rules

Before using Zepto tools, call `status` or `list_tools`. If `status` reports
missing or expired credentials, use `auth_start`, complete browser OAuth, and
then call `list_tools`.

Search and order-history reads are low risk, but still summarize results before
taking any cart or checkout action.

For `call_tool`, set `risk` truthfully:
- `read`: product search, order history, current cart, payment-method listing.
- `cart_mutation`: add, update, remove, clear cart, apply coupon, address edits.
- `checkout_or_payment`: place order, checkout, reserve/pay, payment link/card/
  UPI/wallet selection.

Never mutate the cart without explicit user intent. For add, update, or remove
operations, summarize the item name, variant/pack size, quantity, and
approximate price when available. The runtime does not trust remote MCP
annotations as policy: a newly discovered tool without an exact local rule,
including a cart mutation, receives conservative one-time Attention approval.

Never place an order, reserve UPI payment, open a payment link, or select a
payment method without a final explicit HITL confirmation. The exact order-tool
policy remains conservative even for a preview with `confirmOrder=false`; only
`confirmOrder=true` crosses the amount-verification and Resource Authority
checkout boundary. Final order placement/payment calls must include delivery
address label or visible address summary, cart contents, total amount when
available, and payment method. Then call with `risk=checkout_or_payment`, a
concise `intent_summary`, and `order_amount_inr` set to the cart total in rupees
you just reviewed; runtime approval policy must surface the final approval in
Attention before the call executes.

`order_amount_inr` is REQUIRED on `risk=checkout_or_payment` calls. The generic
MCP commerce boundary cross-checks it against the live cart and then reserves
the verified rupee amount through Resource Authority. Missing budgets,
unverifiable carts, and amount mismatches fail closed. Always re-read the cart
total immediately before checkout and pass it verbatim.

If products are ambiguous, unavailable, have substitutions, or have surprising
price differences, pause and present choices. Do not silently substitute brand,
pack size, dietary restriction, medicine, baby product, or high-value item.

For images or recipes, translate the request into an editable shopping list
first, then search/add only after the user accepts the list.

## Useful Prompts

- "Search Zepto for 1L toned milk near my default address."
- "Add two Amul milk 1L packs to cart, but do not place the order yet."
- "Show my current cart and available payment methods."
- "Reorder my last order, then show me the cart before checkout."
- "Build a paneer butter masala shopping list for four people and ask before
  adding anything."

## Failure Modes

- Browser does not open: rerun `auth_start`; the product reports whether the
  trusted browser launcher accepted the request without exposing the URL.
- OAuth times out: rerun `auth_start` and complete phone/OTP login within the
  bounded callback window.
- Redirect is rejected: confirm the local runtime API is reachable on the
  fixed loopback callback route and begin a fresh authorization.
- Tool calls return no inventory: ask for city/address context or search a
  broader term.
- Payment flow requires human action: hand off clearly; never ask for card,
  UPI PIN, OTP, or other payment secrets in chat.
