//! A Debug Adapter Protocol client. Like the LSP client, it runs on plain threads and futures
//! channels with no UI framework, so sessions can be driven headless against fake and real
//! adapters.

mod client;
#[cfg(any(test, feature = "test-support"))]
pub mod fake;
pub mod session;
mod transport;
pub mod types;

pub use client::{AdapterCommand, Connected, DebugAdapter, Incoming, RequestError, RequestFuture};
pub use session::{Change, FileBreakpoints, LaunchRequest, RunState, SessionState, Started, Step};
pub use transport::{read_message, write_message};
pub use types::*;

#[cfg(test)]
mod tests;
