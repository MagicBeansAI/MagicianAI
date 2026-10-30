---
name: swiggy-mcp
version: 0.1.3
description: |
  Toolskill for Swiggy's official remote MCP servers for Food, Instamart,
  Dineout and Scenes events. Use it to authenticate with browser OAuth, list live Swiggy MCP
  tools, search restaurants or products, manage carts, track orders, or place
  final orders/bookings only after runtime HITL approval.
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
        reason: >-
          Live canaries need separately authorized service accounts; no fixed
          non-interactive event/cart fixture is supplied.
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
            scenes: https://mcp.swiggy.com/scenes
          default_endpoint_alias: food
          oauth:
            authorization_issuer: https://mcp.swiggy.com/auth
            scopes: [mcp:tools, mcp:resources, mcp:prompts]
          tool_policies:
            discover_events: { risk: read_only }
            search_events: { risk: read_only }
            get_event_details: { risk: read_only }
            get_event_venue_info: { risk: read_only }
            get_events_collection: { risk: read_only }
            get_saved_locations: { risk: read_only }
            list_event_shows: { risk: read_only }
            get_event_cart: { risk: read_only }
            get_event_booking_status: { risk: read_only }
            get_payment_options: { risk: read_only }
            create_event_cart:
              risk: external_side_effect
              trusted_description: Prepare an event ticket cart and return its authoritative quote without placing an order.
            check_payment_status:
              risk: external_side_effect
              trusted_description: Inspect an existing payment attempt; successful payment may auto-confirm its existing order, without starting a new charge.
            confirm_order:
              risk: external_side_effect
              trusted_description: Complete an existing paid order using its returned order and payment identifiers; this does not start another purchase.
            place_event_order:
              risk: commerce
              trusted_description: Place the exact reviewed event ticket cart using its native paid or server-enforced zero-price checkout contract.
              additional_approvals: [commerce_checkout]
              additional_resource_scopes: [commerce]
              additional_required_resource_authorities: [resource_authority]
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
            final_tools: [place_food_order, checkout, book_table, place_event_order]
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
      categories: [commerce, shopping, food, events]
      composition_category: commerce_operations
      profile_parameter:
        name: profile
        enum_values: [personal]
      mcp_endpoint_parameter: service
    mcp_bindings:
      tools:
        create_event_cart:
          commerce_quote:
            endpoint_aliases: [scenes]
            id_pointer: /data/cartKey
            total_pointers: [/data/cart/bill/billToPay, /data/cart/bill/dineoutPricingInfo/billToPay]
            money_unit: major
            success_pointer: /success
            mode_pointer: /data/checkoutMode
            allowed_modes: [UPI, FREE]
            free_mode: FREE
            free_flag_pointer: /data/cart/bill/skipPayment
            context_arguments: [/venueId, /showId, /ticketId, /showTime, /guestCount, /latitude, /longitude]
        place_event_order:
          commerce_checkout:
            endpoint_aliases: [scenes]
            quote_tool: create_event_cart
            id_argument: /cartKey
            mode_argument: /paymentMethod
            success_pointer: /success
            server_enforces_zero_price: true
---

# Swiggy MCP

Swiggy exposes four hosted remote MCP servers for Indian commerce workflows:

- Food: `https://mcp.swiggy.com/food`
- Instamart: `https://mcp.swiggy.com/im`
- Dineout: `https://mcp.swiggy.com/dineout`
- Scenes: `https://mcp.swiggy.com/scenes`

The governed runtime connects to those endpoints directly through the official
Rust MCP SDK. First use opens browser OAuth through Magican's scoped Auth Broker;
tokens remain in the encrypted principal/workspace vault and each service is an
exact, separate OAuth resource binding.

Official sources:
- Docs: https://mcp.swiggy.com/builders/docs/
- Manifest: https://github.com/Swiggy/swiggy-mcp-server-manifest

## Tool Actions

Inspect credentials with `status` or discover tools with `list_tools` for the
service you need. If sign-in is needed, call `auth_start` explicitly; the Auth
Broker opens the browser, receives the fixed callback, and the SDK verifies access. Never ask the user to
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
- `scenes`: event discovery, venues, show schedules, ticket carts and approved checkout.

## Scenes checkout

Use live `get_event_details` and `list_event_shows` identifiers. Copy
`show_time_unix` directly as `showTime`; do not convert a display date yourself.
Confirm the show, ticket type and guest count, then call `create_event_cart`.
Show the returned venue, date/time, tickets, fees and total to the user.
`checkoutMode=UNAVAILABLE` stops the flow. Do not fetch the cart again or use a
model-supplied quote as evidence.

The runtime retains the successful native quote for ten minutes, bound to this
scope, endpoint, credential session, policy, cart and full event selection.
For `place_event_order`, echo **all seven** event-selection arguments from
`create_event_cart` (also for paid tickets), plus its exact `cartKey` and
`paymentMethod` matching `checkoutMode`. Set `risk=checkout_or_payment`, a
meaningful `intent_summary` and `order_amount_inr` equal to that quote. A stale,
missing, used or mismatched quote requires recreating and reviewing the cart.
Quotes and single-use claims survive worker handoff or backend restart within
that window. Changing login or policy requires a new quote.

- `UPI`: after the user's confirmation, get payment options, copy `intentApp`
  exactly or request a QR code, then place the reviewed order. Positive amounts
  pass through Resource Authority once, at payment initiation. Present the
  returned payment link/QR for the user to pay; `PENDING_PAYMENT` is not a booking.
- `FREE`: use only a native zero-total `checkoutMode=FREE` quote and
  `order_amount_inr=0`. The final provider call recreates/reprices the selection
  and atomically rejects anything that is no longer free. Explicit transaction
  approval still applies; no money is reserved for a verified zero-price order.

Payment-status and confirmation calls continue an existing attempt, rather than
starting a second purchase or debit. Use only the returned `paasId` and `orderId`.
Poll no faster than the returned interval and stop at terminal status. If the
status already says `confirmed=true`, do not call `confirm_order` again. Confirm
manually only after terminal payment success when it has not auto-confirmed.
Failed, cancelled, refund-initiated or cart-changed payments must not be confirmed.
Use `get_event_booking_status` to verify a `PROCESSING` booking. After an ambiguous
checkout response, inspect the existing attempt or ask the user; do not place a
second order. Creating a cart, opening payment or returning a link is not proof
of a completed booking.

Endpoint URLs are not model inputs. If Swiggy changes an endpoint, update the
reviewed alias map in this skill contract.

## Operating Rules

`status` only observes saved credentials; it does not open sign-in. For an
explicit sign-in retry, call `auth_start` once for the selected service. A
client-registration failure happens before a browser or sign-in card exists;
report that blocker rather than claiming that a popup was blocked.

Before using Swiggy tools, call `status` or `list_tools` for the selected
service. If `status` reports missing or expired credentials, use `auth_start`,
complete browser OAuth, and then call `list_tools`.

For `call_tool`, set `risk` truthfully:

- `read`: search, menu/product details, current cart, saved addresses, order
  details, order status, booking status, available slots.
- `cart_mutation`: add/update/remove/clear cart, apply coupons, create carts,
  create/delete delivery addresses.
- `checkout_or_payment`: Food `place_food_order`, Instamart `checkout`,
  Dineout `book_table`, Scenes `place_event_order`, or any future purchase finalizer.

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
MCP commerce boundary cross-checks it against the live cart (or Scenes' bound native quote) and then reserves
the verified rupee amount through Resource Authority. Missing budgets, amount
mismatches, or unverifiable Food/Instamart carts fail closed. Dineout is the one
reviewed cartless endpoint alias and still requires the booking total. Scenes
alone supports a zero amount when its bound quote and native checkout are free.

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
