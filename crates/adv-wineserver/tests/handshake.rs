//! A client connects to the in-process server and receives its request pipe.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

#[path = "common/handshake.rs"]
mod common;

// spec: WINE-SRV-1, SRV-CHAN-1
#[test]
fn client_receives_request_pipe_and_server_exits_on_its_clock() {
    common::handshake(false);
}
