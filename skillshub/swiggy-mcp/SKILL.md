---
name: swiggy-mcp
version: 0.1.0
description: |
  Toolskill for Swiggy's official remote MCP servers for Food, Instamart, and
  Dineout. Use it to authenticate with browser OAuth, list live Swiggy MCP
  tools, search restaurants or products, manage carts, track orders, or place
  final orders/bookings only after runtime HITL approval.
metadata:
  magician:
    setup: { definition: governed-mcp-oauth }
    skill_type: tool
    user_invocable: true
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: >-
          every action moves toward placing a real food order against the
          operator's account, and the package declares no read-only action.
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      runtime:
        protocol: mcp
        transport:
          kind: streamable_http
          endpoint: https://mcp.swiggy.com/food
        discovery:
          namespace: swiggy
          endpoint_aliases:
            food: https://mcp.swiggy.com/food
            instamart: https://mcp.swiggy.com/im
            dineout: https://mcp.swiggy.com/dineout
          default_endpoint_alias: food
          oauth:
            authorization_issuer: https://mcp.swiggy.com/auth
            scopes: [mcp:tools, mcp:resources, mcp:prompts]
          tool_policies:
            place_food_order:
              risk: commerce
              trusted_description: Place the reviewed food order after final product approval and INR resource authorization.
              additional_approvals: [commerce_checkout]
              additional_resource_scopes: [commerce]
              additional_required_resource_authorities: [resource_authority]
            checkout:
              risk: commerce
              trusted_description: Finalize the reviewed Instamart cart after final product approval and INR resource authorization.
              additional_approvals: [commerce_checkout]
              additional_resource_scopes: [commerce]
              additional_required_resource_authorities: [resource_authority]
            book_table:
              risk: commerce
              trusted_description: Book the reviewed Dineout table after final product approval and INR resource authorization.
              additional_approvals: [commerce_checkout]
              additional_resource_scopes: [commerce]
              additional_required_resource_authorities: [resource_authority]
          commerce:
            commodity: INR
            resource_scope: commerce
            required_resource_authority: resource_authority
            amount_parameter: order_amount_inr
            final_tools: [place_food_order, checkout, book_table]
            checkout_name_terms: [checkout, payment_link, reserve_pay, purchase]
            cart_name_terms: [cart]
            cart_read_verb_terms: [get, view, fetch, show, summary, detail, current, list]
            total_field_priority: [finalamount, totalamount, billtotal, total, amount]
            cart_amount_unit: major
            absolute_tolerance_minor: 2500
            percentage_tolerance_bps: 500
            cartless_endpoint_aliases: [dineout]
            allow_unverified_cart: false
        limits:
          timeout_secs: 90
          stdout_bytes: 33554432
          stderr_bytes: 1048576
      auth:
        kind: oauth_session
        requirement: required
        provider: swiggy-mcp
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
      mcp_endpoint_parameter: service
---

# Swiggy MCP

Swiggy exposes three hosted remote MCP servers for Indian commerce workflows:

- Food: `https://mcp.swiggy.com/food`
- Instamart: `https://mcp.swiggy.com/im`
- Dineout: `https://mcp.swiggy.com/dineout`

The governed runtime connects to those endpoints directly through the official
Rust MCP SDK. First use opens browser OAuth through Magican's scoped Auth Broker;
tokens remain in the encrypted principal/workspace vault and each service is an
exact, separate OAuth resource binding.

Live OAuth/tool discovery on 2026-06-09 verified that these are three distinct
MCP servers with separate OAuth resource registrations and token bindings:
Food returned `swiggy-food-mcp-server` with 14 tools, Instamart returned
`swiggy-instamart-mcp-server` with 10 tools, and Dineout returned
`swiggy-dineout-mcp-server` with 8 tools.

Official sources:
- Docs: https://mcp.swiggy.com/builders/docs/
- Manifest: https://github.com/Swiggy/swiggy-mcp-server-manifest

## Tool Actions

Use `auth_start` or `list_tools` first for the service you need. If credentials
are missing or expired, the Auth Broker opens the user's browser, receives the
fixed localhost callback, and the SDK verifies access. Never ask the user to
share an OTP, card detail, UPI PIN, or any other secret in chat.

- `status`: inspect the exact scoped OAuth binding without exposing a token.
- `auth_start`: start browser OAuth for the exact service/profile binding.
- `list_tools`: list live remote Swiggy MCP tools and input schemas.
- `call_tool`: call a remote Swiggy MCP tool by name with `arguments_json`.
- `clear_auth`: delete scoped OAuth state for one service/profile binding.

## Service Selection

Set `service` truthfully:

- `food`: restaurant search, menus, food cart, food orders, food tracking.
- `instamart`: groceries and quick-commerce product/cart/order flows.
- `dineout`: restaurant discovery, slot availability, and table bookings.

Endpoint URLs are not model inputs. If Swiggy changes an endpoint, update the
reviewed alias map in this skill contract.

## Operating Rules

Before using Swiggy tools, call `status` or `list_tools` for the selected
service. If `status` reports missing or expired credentials, use `auth_start`,
complete browser OAuth, and then call `list_tools`.

For `call_tool`, set `risk` truthfully:

- `read`: search, menu/product details, current cart, saved addresses, order
  details, order status, booking status, available slots.
- `cart_mutation`: add/update/remove/clear cart, apply coupons, create carts,
  create/delete delivery addresses.
- `checkout_or_payment`: Food `place_food_order`, Instamart `checkout`,
  Dineout `book_table`, or any future payment/order/reservation finalizer.

Never mutate a cart without explicit user intent. For add, update, remove, or
coupon operations, summarize item/restaurant, variant or customization,
quantity, address context, and approximate price when available. The runtime
does not trust remote MCP annotations as policy: a newly discovered tool
without an exact local rule, including a cart mutation, receives conservative
one-time Attention approval.

Never place a food order, run Instamart checkout, book a table, or initiate any
payment/bill action without final explicit HITL confirmation. Before the final
call, show the user the complete order or booking summary: delivery address or
restaurant address, cart/items or party size/slot, total amount when available,
and payment method. Use only the payment methods surfaced by the preceding live
cart or payment-options tool; do not invent COD/UPI/card availability. Then call
with `risk=checkout_or_payment`, a concise `intent_summary`, and
`order_amount_inr` set to the order/booking total in rupees you just reviewed;
Runtime approval policy must surface the final approval in Attention before the
call executes.

`order_amount_inr` is REQUIRED on `risk=checkout_or_payment` calls. The generic
MCP commerce boundary cross-checks it against the live cart and then reserves
the verified rupee amount through Resource Authority. Missing budgets, amount
mismatches, or unverifiable Food/Instamart carts fail closed. Dineout is the one
reviewed cartless endpoint alias and still requires the booking total.

If a UPI or paid booking flow returns a pending-payment state, hand off clearly
for human payment completion in the Swiggy/UPI surface and do not report success
until a live Swiggy tool confirms completion. Do not promise cancellation,
refunds, or unsupported paid booking behavior. If a cancellation is requested,
follow the remote tool guidance or tell the user to use Swiggy customer
support/app when no matching tool exists.

If products, restaurants, slots, substitutions, customizations, or price
differences are ambiguous, pause and present choices. Do not silently substitute
brand, pack size, cuisine constraint, dietary restriction, medicine, baby
product, or high-value item.

## Useful Prompts

- "Use Swiggy Food to find biryani near my saved home address."
- "Search Instamart for bananas near home and show prices."
- "Add these groceries to Instamart cart, then show me the total before checkout."
- "Find Italian restaurants on Dineout for Saturday 8 PM for two people."
- "Track my active Swiggy Food order."

## Failure Modes

- Browser does not open: rerun `auth_start`; the product reports whether the
  trusted browser launcher accepted the request without exposing the URL.
- OAuth times out: rerun `auth_start` and complete phone/OTP login within the
  bounded callback window.
- Redirect is rejected: confirm the local runtime API is reachable on the
  fixed loopback callback route and begin a fresh authorization.
- 401 after a few days: Swiggy's access token may have expired; rerun
  authorization for the service.
- "Tool not found": check that `service` matches the tool domain, for example
  Food `place_food_order`, Instamart `checkout`, or Dineout `book_table`.
