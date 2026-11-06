//! A Language Server Protocol client. Runs on plain threads and futures channels, with no
//! dependency on the UI framework, so it can be tested on its own against fake and real servers.

mod client;
mod transport;
pub mod uri;

pub use client::{
    Incoming, LanguageServer, REQUEST_CANCELLED, RequestError, ServerOptions, client_capabilities,
};
pub use lsp_types;
pub use transport::{read_message, write_message};
pub use uri::{path_to_uri, uri_to_path};

#[cfg(test)]
mod tests;
