//! Dormant S3-compatible object and dataset adapters.
//!
//! Constructible only from an explicit remote profile or a hermetic test
//! backend. `magician-bin` default startup must not depend on this crate.

mod backend;
mod config;
mod dataset;
mod http;
mod memory;
mod object;
mod sigv4;

pub use backend::BlobStore;
pub use config::{open_from_profile, RemoteOpenOptions, RemoteStores};
pub use dataset::S3DatasetStore;
pub use http::{S3HttpBlobStore, S3HttpSettings};
pub use memory::MemoryBlobStore;
pub use object::S3ObjectStore;
pub use sigv4::{sign_s3_request, SignInput};
