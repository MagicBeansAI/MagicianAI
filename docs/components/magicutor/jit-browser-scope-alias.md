# Trusted credential delivery CDP scope alias

The browser endpoint accepts `magicvault-scope-<lowercase hex session id>` as a
reversible spelling of an existing scoped session. This allows the shared
MagicVault browser adapter's restricted endpoint grammar to reach Magician
session IDs containing underscores, including the browser transport-ceiling
suffix. It decodes before normal owned-tab routing, preserving exactly the
same window and tab ownership checks as the ordinary session path.

Aliases accept only nonempty ASCII session IDs of at most 128 bytes with
letters, digits, hyphens or underscores. The legacy unscoped `magicutor-proxy`
ID is forbidden. Malformed reserved aliases return HTTP 400, with no fallback
to a newly created scope. This is an encoding, not an authentication token or
new permission. Runtime endpoint configuration and existing local CDP access
policy still apply. Deploy Magicutor with the updated Magician runtime for
[one-time credential fill](../magician/jit-browser-credentials.md).
