---
name: shopify-mcp
version: 0.1.1
description: |
  Discover products across Shopify merchants using the official public UCP
  Global Catalog MCP. Compare live offers and obtain merchant checkout links.
  Uses this installation's automatically published public UCP profile;
  this connector does not administer stores or complete purchases.
metadata:
  magician:
    skill_type: tool
    user_invocable: true
    runtime_canary:
      schema_version: tool-runtime.canary.v1
      exempt:
        reason: Catalog calls require this installation's public HTTPS remote-access setup; discovery does not need credentials.
    mcp_bindings:
      public_profile:
        argument_pointer: /meta/ucp-agent/profile
        document:
          ucp:
            version: '2026-08-25'
            services:
              dev.ucp.shopping:
                - version: '2026-08-25'
                  spec: https://ucp.dev/2026-08-25/specification/overview
                  transport: mcp
                  schema: https://ucp.dev/2026-08-25/services/shopping/mcp.openrpc.json
            capabilities:
              dev.ucp.shopping.catalog.search:
                - version: '2026-08-25'
                  spec: https://ucp.dev/2026-08-25/specification/catalog/search
                  schema: https://ucp.dev/2026-08-25/schemas/shopping/catalog_search.json
              dev.ucp.shopping.catalog.lookup:
                - version: '2026-08-25'
                  spec: https://ucp.dev/2026-08-25/specification/catalog/lookup
                  schema: https://ucp.dev/2026-08-25/schemas/shopping/catalog_lookup.json
              dev.shopify.catalog.global:
                - version: '2026-08-25'
                  spec: https://shopify.dev/docs/agents/catalog/global-catalog
                  schema: https://shopify.dev/ucp/schemas/2026-08-25/shopify_catalog_global.json
                  extends: [dev.ucp.shopping.catalog.search, dev.ucp.shopping.catalog.lookup]
            payment_handlers: {}
    runtime_contract:
      schema_version: tool-runtime.skill-runtime.v1
      runtime:
        protocol: mcp
        transport:
          kind: streamable_http
          endpoint: https://catalog.shopify.com/api/ucp/mcp
        discovery:
          namespace: shopify
          allow_tools: [search_catalog, lookup_catalog, get_product]
          tool_policies:
            search_catalog:
              risk: read_only
              trusted_description: Search live Shopify merchant product offers.
            lookup_catalog:
              risk: read_only
              trusted_description: Resolve product identifiers against the live Shopify catalog.
            get_product:
              risk: read_only
              trusted_description: Read live product variants, seller offers and checkout links.
        limits:
          timeout_secs: 90
          stdout_bytes: 33554432
          stderr_bytes: 1048576
      auth:
        kind: none
        requirement: none
      policy_floor:
        approval: ordinary
        resource_scopes: [external_mcp]
    runtime_catalog:
      categories: [commerce, shopping]
      composition_category: commerce_operations
---

# Shopify Catalog MCP

Use `list_tools`, then `call_tool` with the exact discovered tool name and
`arguments_json`. This is the buyer-facing Global Catalog, not Shopify's
developer-documentation MCP or a merchant Admin API. No consumer OAuth or API
key is needed. `status` reports that authentication is unnecessary; use
`list_tools` to actually verify connectivity.

## Agent profile

Magican publishes the literal profile above on this installation's public HTTPS
hostname and automatically supplies `meta.ucp-agent.profile` before the call is
approved and sent. Each installation owns its URL and local document; no shared
operator identity, profile editing or manual URL entry is required. The document
contains catalog capabilities only, never credentials or customer information.

Omit `meta.ucp-agent.profile` from model-authored arguments. Do not substitute
another application's profile or Shopify's test profiles. If public HTTPS is
not configured, guide the owner through **Reach this computer remotely** in
setup. A localhost URL or an Access login page cannot serve Shopify's fetch.
If the provider cannot fetch the profile, report that setup problem once;
do not repeatedly retry catalog searches.

Example input shape (the runtime supplies the profile URL):

```json
{
  "catalog": {
    "query": "cotton shirts",
    "context": {"address_country": "IN", "currency": "INR"},
    "filters": {"ships_to": {"country": "IN"}, "available": true}
  }
}
```

Use the buyer's requested destination and currency. Ask only for essential
missing delivery context. Prices in filters are **minor currency units**; do not
compare currencies as though they were identical. Use live product/variant IDs
and seller checkout links from results. Do not cache catalog results or
download/cache merchant images server-side.
Inline image data in `catalog.like` must be base64-encoded.

## Checkout boundary

Only the three reviewed catalog tools are admitted. Merchant-specific MCP cart,
checkout, order and Admin APIs are not connected by this package: a merchant URL
in a result does not grant a new MCP endpoint. Offer the returned checkout link
or use the separately governed browser with explicit buyer intent. Never claim
an order was placed from a catalog result or checkout URL.

Official sources:
- https://shopify.dev/docs/agents/catalog/global-catalog
- https://shopify.dev/docs/agents/profiles
