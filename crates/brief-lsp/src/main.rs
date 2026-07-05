// mimalloc roughly doubles parse/emit throughput on allocation-heavy
// documents (the AST is built from many small owned strings and vecs).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod backend;

use backend::Backend;
use tower_lsp_server::{LspService, Server};
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() {
    // All tracing output goes to stderr so that stdout stays clean for the
    // JSON-RPC transport. Never write to stdout directly.
    fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| Backend::new(client));
    Server::new(stdin, stdout, socket).serve(service).await;
}
