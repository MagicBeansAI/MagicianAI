//! Task 20 — compute placement is independent of storage placement.
//!
//! Default startup stays `local_embedded`. Remote-durable profiles are
//! proven with hermetic adapters and two engine identities against one
//! store. Device-bound automation is unavailable without the device bridge.

mod device;

pub use device::{
    device_bound_availability, DeviceAvailability, DeviceBoundSurface, DEVICE_BRIDGE_REQUIRED,
};

#[cfg(test)]
mod scenarios;
