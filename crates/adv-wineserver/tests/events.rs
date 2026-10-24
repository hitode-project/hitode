//! Ordinary server descriptors do not enter full-array kernel waits.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

#[path = "common/handshake.rs"]
mod common;

// spec: SRV-EVENT-1, WINE-SRV-1
#[test]
fn requests_with_many_ordinary_descriptors_use_incremental_events() {
    common::handshake(true);
}
