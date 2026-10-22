//! `brief-lsp`: language server for the Brief markup language, speaking
//! LSP over stdio.

mod analysis;
mod backend;

use std::io;

use backend::Backend;
use mimalloc::MiMalloc;
use tower_lsp_server::{LspService, Server};
use tracing_subscriber::EnvFilter;

// mimalloc roughly doubles parse/emit throughput on allocation-heavy
// documents (the AST is built from many small owned strings and vecs).
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main]
async fn main() {
    // All tracing output goes to stderr so that stdout stays clean for the
    // JSON-RPC transport. Never write to stdout directly.
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
