// mimalloc roughly doubles parse/emit throughput on allocation-heavy
// documents (the AST is built from many small owned strings and vecs).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use brief_web::scaffold::ScaffoldOpts;
use brief_web::server::{ReloadBroadcaster, ReloadEvent, ServerOptions};
use brief_web::watch::{default_watch_paths, watch};
use brief_web::{Builder, scaffold, server};
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

#[derive(Parser)]
#[command(name = "brief-web", version, about = "Static-site generator for Brief")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a new brief-web project.
    Init {
        /// Target directory (defaults to current directory).
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Generate a multi-page skeleton with `SUMMARY.brf`.
        #[arg(long)]
        multi: bool,
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
    /// Build the site once into `dist/`.
    Build {
        /// Project directory.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Build, then serve the site with live reload.
    Serve {
        /// Project directory.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Override host (defaults to `book.toml`'s `[server]` setting).
        #[arg(long)]
        host: Option<String>,
        /// Override port (defaults to `book.toml`'s `[server]` setting).
        #[arg(long)]
        port: Option<u16>,
        /// Skip the watcher; serve the existing `dist/` as-is.
        #[arg(long)]
        no_watch: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { path, multi, force } => init(path, multi, force),
        Cmd::Build { path } => build(path),
        Cmd::Serve {
            path,
            host,
            port,
            no_watch,
        } => serve(path, host, port, no_watch),
    }
}

fn init(path: PathBuf, multi: bool, force: bool) -> ExitCode {
    match scaffold::create(
        &path,
        &ScaffoldOpts {
            multi_page: multi,
            force,
        },
    ) {
        Ok(()) => {
            eprintln!(
                "brief-web: scaffolded project at {}{}",
                path.display(),
                if multi { " (multi-page)" } else { "" }
            );
            eprintln!("Next: cd {} && brief-web serve", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("brief-web: {}", e);
            ExitCode::from(2)
        }
    }
}

fn build(path: PathBuf) -> ExitCode {
    let builder = match Builder::new(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("brief-web: {}", e);
            return ExitCode::from(2);
        }
    };
    match builder.build() {
        Ok(o) => {
            for w in &o.warnings {
                eprintln!("brief-web: warning: {}", w);
            }
            eprintln!(
                "brief-web: built {} page{} → {}",
                o.pages_built,
                if o.pages_built == 1 { "" } else { "s" },
                builder.out_dir().display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("brief-web: build failed:\n{}", e);
            ExitCode::from(1)
        }
    }
}

fn serve(
    path: PathBuf,
    host_override: Option<String>,
    port_override: Option<u16>,
    no_watch: bool,
) -> ExitCode {
    let mut builder = match Builder::new(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("brief-web: {}", e);
            return ExitCode::from(2);
        }
    };
    builder.inject_reload = !no_watch;

    let initial = match builder.build() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("brief-web: initial build failed:\n{}", e);
            return ExitCode::from(1);
        }
    };
    for w in &initial.warnings {
        eprintln!("brief-web: warning: {}", w);
    }
    eprintln!(
        "brief-web: built {} page{} for serve",
        initial.pages_built,
        if initial.pages_built == 1 { "" } else { "s" }
    );

    let host = host_override.unwrap_or_else(|| builder.config.server.host.clone());
    let port = port_override.unwrap_or(builder.config.server.port);
    let out_root = builder.out_dir();
    let bcast = ReloadBroadcaster::new();

    if !no_watch {
        let shared = Arc::new(Mutex::new(builder.clone()));
        let project_dir = builder.project_dir.clone();
        let src_dir = builder.src_dir();
        let theme_dir = builder.theme_dir();
        let watch_paths = default_watch_paths(&project_dir, &src_dir, theme_dir.as_deref());
        let bcast_for_watch = bcast.clone();
        let handles = watch(&watch_paths, move || {
            let b = shared.lock().unwrap();
            match b.build() {
                Ok(o) => {
                    for w in &o.warnings {
                        eprintln!("brief-web: warning: {}", w);
                    }
                    eprintln!("brief-web: rebuilt {} pages", o.pages_built);
                    bcast_for_watch.broadcast(ReloadEvent::Reload);
                }
                Err(e) => {
                    eprintln!("brief-web: rebuild failed:\n{}", e);
                    bcast_for_watch.broadcast(ReloadEvent::BuildError(e));
                }
            }
        });
        // Hold the watcher alive for the full server lifetime.
        Box::leak(Box::new(handles));
    }

    let opts = ServerOptions {
        host,
        port,
        root: out_root,
        broadcaster: bcast,
    };
    if let Err(e) = server::run(opts) {
        eprintln!("brief-web: server error: {}", e);
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}
