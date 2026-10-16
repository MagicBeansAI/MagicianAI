#![cfg(test)]
//! Exercise the production round progression without a provider or host I/O.
#[path = "../../../magician/src/magician_v2/apps/contextual_round.rs"]
#[allow(dead_code)]
mod contextual_round;

#[path = "../../../magician/src/magician_v2/apps/concurrent_progress.rs"]
mod concurrent_progress;

#[path = "../../../magician/src/magician_v2/apps/contextual_round_program.rs"]
#[allow(dead_code)]
mod contextual_round_program;

#[path = "../../../magician/src/magician_v2/apps/contextual_round_declaration.rs"]
#[allow(dead_code)]
mod contextual_round_declaration;

#[path = "../../../magician/src/magician_v2/apps/store_transaction.rs"]
#[allow(dead_code)]
mod store_transaction;

#[path = "../../../magician/src/magician_v2/apps/linked_text_rows.rs"]
#[allow(dead_code)]
mod linked_text_rows;

mod store_transactions;
mod town_square;

mod host_store_transactions;

#[path = "../../../magician/src/magician_v2/apps/control_encoding.rs"]
mod control_encoding;
