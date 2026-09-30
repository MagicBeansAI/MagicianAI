# document-to-markdown-cli

Standalone command-line adapter over the pinned AnyDoc Rust library. AnyDoc is
library-only, so this crate supplies the stable, bounded JSON CLI contract used
by the governed `document-to-markdown` skill.

The crate is deliberately independent of skill discovery and Magician runtime
code. `make -C skillshub setup-document-to-markdown` builds it in the shared
Cargo target directory using the requested `PROFILE=debug|release` and creates
the ignored `skillshub/document-to-markdown/bin/document-to-markdown` link to
the matching artifact. The root debug and release build targets select the
matching profile explicitly. The skill then behaves exactly like any other
executable plus `SKILL.md` package.

Magician invokes the standalone process behind the governed executable boundary,
so a parser failure cannot unwind the backend. Inputs are limited to 128 MiB,
inline Markdown is bounded by characters and encoded bytes, output and asset
destinations are create-only, and an asset tree is published with one atomic
no-replace directory operation. Structured nonzero-exit errors retain a bounded
stable code and message without exposing document contents. URL-based
content acquisition applies the shared reader's stricter 32 MiB HTTP ceiling
before the local process receives its scratch file.

The governed package additionally declares a 4 GiB process-address-space
ceiling. The shared process owner enforces that limit before exec and reserves
it against an 8 GiB global governed-memory budget, so at most two worst-case
AnyDoc conversions run concurrently. This bounds AnyDoc's own decompression and
document-model allocations without moving parser-specific logic into Magician.
