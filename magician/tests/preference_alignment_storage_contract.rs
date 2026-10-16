//! Run the storage modules' production-source regressions as a focused contract.
//! Uses the real host's memory, file locks and durable I/O while targeting
//! these three storage modules independently of the full library unit suite.

pub mod magician_v2 {
    pub use magician::magician_v2::*;
    pub mod preference_alignment {
        pub use magician::magician_v2::preference_alignment::*;
        // Keep crate-qualified test imports on the same source modules as
        // sibling-qualified imports, including their shared history types.
        pub use crate::{admission, history, wording_grant};
    }
}
pub use magician::magician_v2::preference_alignment::{
    actor, generation, intention, policy, service, sources,
};

#[path = "../src/magician_v2/preference_alignment/admission.rs"]
pub mod admission;
#[path = "../src/magician_v2/preference_alignment/history.rs"]
pub mod history;
#[path = "../src/magician_v2/preference_alignment/wording_grant.rs"]
pub mod wording_grant;
