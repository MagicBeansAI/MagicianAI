//! Counterparty identity — re-exported from the magician lib, where the
//! types, consumers, and store now live (chat's inbound authority is their
//! primary consumer).
pub use magician::magician_v2::counterparty_consumers as consumers;
pub use magician::magician_v2::counterparty_store as store;
pub use magician::magician_v2::counterparty_types as types;
