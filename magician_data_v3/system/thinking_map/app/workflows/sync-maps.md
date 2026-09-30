Synchronize one bounded page through the shared deterministic reconciliation
recipe. The runtime forwards only the reviewed lifecycle and limit inputs to
thinking_maps_data.list_maps, preserving numeric types and enforcing the
provider's 25-row limit. An empty page is a successful no-op. Records outside
that page remain untouched.

When map_id is present, the recipe also reads exactly that document through
thinking_maps_data.read_map. Without map_id, no snapshot read runs. An invalid
or empty supplied ID fails the host argument proof before I/O. No model selects
an ID or decides which read to perform.

The recipe copies summary fields verbatim and serializes the complete returned
map document into the snapshot field, with a reviewed 262144-byte bound. It
does not summarize or truncate the document. Both reads must succeed before
one atomic App-store transaction; a failed optional read preserves all existing
records. Stable natural-key IDs and exact record-revision fences preserve
existing identities and prevent stale overwrites. The map substrate is read-only.
