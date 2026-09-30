//! Generic YAML-driven CLI dispatcher for inner-loop tools.
//!
//! Used for pack-defined inner-looped packs (gmail, csvkit, metabase_explore,
//! ...) where the per-primitive translation is declared in YAML — no Rust code
//! per tool. The pack's optional `implementation.command: Vec<String>`
//! overrides the default base (which is just the pack name).
//!
//! For each primitive:
//! - `native_action_schemas.<tool>.argv` can provide exact argv tokens after
//!   the pack-level base.
//! - `arg_mappings` can emit positionals, flags, fixed args, env-backed flags,
//!   or shell-like lexical splitting without invoking a shell.
//! - If no action-specific mapping is declared, the fallback remains
//!   `<base> <primitive_name> [--kebab-case value...]`.
//!
//! See [`args::build_argv`] for the full argv shape.

pub mod args;
pub mod dispatcher;

pub use dispatcher::CliTemplateDispatcher;
