// mimalloc roughly doubles parse/emit throughput on allocation-heavy
// documents (the AST is built from many small owned strings and vecs).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use brief_web::scaffold::ScaffoldOpts;
use brief_web::server::{ReloadBroadcaster, ReloadEvent, ServerOptions};
use brief_web::watch::{default_watch_paths, watch};
use brief_web::{BuildError, BuildOutcome, Builder, scaffold, server};
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
        /// Fail the build on warnings as well as errors.
        #[arg(long)]
        deny_warnings: bool,
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
        /// Fail builds on warnings as well as errors.
        #[arg(long)]
        deny_warnings: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Init { path, multi, force } => init(path, multi, force),
        Cmd::Build {
            path,
            deny_warnings,
        } => build(path, deny_warnings),
        Cmd::Serve {
            path,
            host,
            port,
            no_watch,
            deny_warnings,
        } => serve(path, host, port, no_watch, deny_warnings),
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

fn build(path: PathBuf, deny_warnings: bool) -> ExitCode {
    let mut builder = match Builder::new(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("brief-web: {}", e);
            return ExitCode::from(2);
        }
    };
    builder.deny_warnings = deny_warnings;
    match builder.build() {
        Ok(o) => {
            print_warnings(&o);
            eprintln!(
                "brief-web: built {} page{} → {}",
                o.pages_built,
                if o.pages_built == 1 { "" } else { "s" },
                builder.out_dir().display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            print_report(&e);
            eprintln!("brief-web: build failed: {}", e.message);
            ExitCode::from(1)
        }
    }
}

fn print_warnings(o: &BuildOutcome) {
    if !o.warnings.is_empty() {
        eprintln!("{}", o.warnings.join("\n"));
    }
}

fn print_report(e: &BuildError) {
    if !e.report.is_empty() {
        eprintln!("{}", e.report);
    }
}

fn serve(
    path: PathBuf,
    host_override: Option<String>,
    port_override: Option<u16>,
    no_watch: bool,
    deny_warnings: bool,
) -> ExitCode {
    let mut builder = match Builder::new(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("brief-web: {}", e);
            return ExitCode::from(2);
        }
    };
    builder.inject_reload = !no_watch;
    builder.deny_warnings = deny_warnings;

    let initial = match builder.build() {
        Ok(o) => o,
        Err(e) => {
            print_report(&e);
            eprintln!("brief-web: initial build failed: {}", e.message);
            return ExitCode::from(1);
        }
    };
    print_warnings(&initial);
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
                    print_warnings(&o);
                    eprintln!("brief-web: rebuilt {} pages", o.pages_built);
                    bcast_for_watch.broadcast(ReloadEvent::Reload);
                }
                // The builder writes nothing until every page is clean, so
                // the server keeps serving the last good output.
                Err(e) => {
                    print_report(&e);
                    eprintln!("brief-web: rebuild failed: {}", e.message);
                    bcast_for_watch.broadcast(ReloadEvent::BuildError(e.message));
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
