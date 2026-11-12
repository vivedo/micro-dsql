//! Integration tests: a micro-dsql server is started in-process on port 0 and driven with
//! `tokio-postgres` (simple and extended protocol) or raw protocol messages.

mod common;
mod crud;
mod errors;
mod extended;
mod session;
mod t1_layer1;
