//! `brief fmt`: rewrite Brief sources in their canonical style, or report
//! which ones would change.

use brief::fmt;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(clap::Args)]
pub struct FmtArgs {
    /// Files and directories to format. Directories are searched
    /// recursively for `.brf` files, skipping hidden directories,
    /// `target/`, `dist/` and `node_modules/`. `-` formats stdin to stdout.
    /// With no paths, formats the whole project: the nearest directory
    /// holding `brief.toml` at or above the current one, else the current
    /// directory.
    paths: Vec<PathBuf>,
    /// Write nothing. List each file that would change, one path per line
    /// on stdout, and exit 1 if there are any.
    #[arg(long, conflicts_with_all = ["diff", "stdout"])]
    check: bool,
    /// Write nothing. Print a unified diff of the changes on stdout and
    /// exit 1 if there are any.
    #[arg(long, conflicts_with = "stdout")]
    diff: bool,
    /// Print the formatted source of a single file on stdout instead of
    /// rewriting it.
    #[arg(long)]
    stdout: bool,
    /// Formatting in place is the default. Still accepted so scripts
    /// written against the old flag keep working.
    #[arg(short = 'w', long, hide = true, conflicts_with_all = ["check", "diff", "stdout"])]
    write: bool,
    /// Sort top-level frontmatter keys alphabetically. Off by default
    /// — re-emitting the TOML loses comment positions.
    #[arg(long)]
    sort_frontmatter: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    InPlace,
    Check,
    Diff,
    Stdout,
}

/// A file to format, and the path to show the user for it.
struct Target {
    path: PathBuf,
    shown: String,
}

pub fn run(args: FmtArgs) -> ExitCode {
    let opts = fmt::Opts {
        sort_frontmatter: args.sort_frontmatter,
    };
    let mode = if args.check {
        Mode::Check
    } else if args.diff {
        Mode::Diff
    } else if args.stdout {
        Mode::Stdout
    } else {
        Mode::InPlace
    };

    if args.paths.iter().any(|p| p.as_os_str() == "-") {
        if args.paths.len() > 1 {
            eprintln!("brief: `-` (stdin) cannot be combined with other paths");
            return ExitCode::from(2);
        }
        return run_stdin(mode, &opts);
    }
    if mode == Mode::Stdout && (args.paths.len() != 1 || args.paths[0].is_dir()) {
        eprintln!("brief: --stdout previews exactly one file");
        return ExitCode::from(2);
    }

    let targets = match collect_targets(&args.paths) {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("brief: {}", msg);
            return ExitCode::from(2);
        }
    };

    let mut stdout = std::io::stdout().lock();
    if mode == Mode::Stdout {
        let target = &targets[0];
        return match std::fs::read_to_string(&target.path) {
            Ok(raw) => finish_stdout(&mut stdout, &fmt::format(&raw, &opts), ExitCode::SUCCESS),
            Err(e) => {
                eprintln!("brief: cannot read {}: {}", target.shown, e);
                ExitCode::from(2)
            }
        };
    }

    let (mut changed, mut unchanged, mut failed) = (0usize, 0usize, 0usize);
    for target in &targets {
        let raw = match std::fs::read_to_string(&target.path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("brief: cannot read {}: {}", target.shown, e);
                failed += 1;
                continue;
            }
        };
        let formatted = fmt::format(&raw, &opts);
        if formatted == raw {
            unchanged += 1;
            continue;
        }
        let reported = match mode {
            Mode::InPlace => {
                match write_atomic(&target.path, &formatted) {
                    Ok(()) => changed += 1,
                    Err(e) => {
                        eprintln!("brief: cannot write {}: {}", target.shown, e);
                        failed += 1;
                    }
                }
                continue;
            }
            Mode::Check => writeln!(stdout, "{}", target.shown),
            Mode::Diff => write!(stdout, "{}", unified_diff(&raw, &formatted, &target.shown)),
            Mode::Stdout => unreachable!("handled above"),
        };
        // With stdout gone there is nowhere left to report to.
        if let Err(e) = reported {
            stdout_error(e);
            return ExitCode::from(2);
        }
        changed += 1;
    }
    if let Err(e) = stdout.flush() {
        stdout_error(e);
        return ExitCode::from(2);
    }

    eprintln!("brief: {}", summary(mode, changed, unchanged, failed));
    if failed > 0 {
        ExitCode::from(2)
    } else if changed > 0 && matches!(mode, Mode::Check | Mode::Diff) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `brief fmt -`: stdin to stdout, for editors and pipes. `--check` and
/// `--diff` report on stdin instead of echoing it.
fn run_stdin(mode: Mode, opts: &fmt::Opts) -> ExitCode {
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("brief: cannot read stdin: {}", e);
        return ExitCode::from(2);
    }
    let formatted = fmt::format(&raw, opts);
    let changed = formatted != raw;
    let report = if changed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    };
    let mut stdout = std::io::stdout().lock();
    match mode {
        Mode::InPlace | Mode::Stdout => finish_stdout(&mut stdout, &formatted, ExitCode::SUCCESS),
        Mode::Check => finish_stdout(&mut stdout, if changed { "<stdin>\n" } else { "" }, report),
        Mode::Diff => finish_stdout(
            &mut stdout,
            &unified_diff(&raw, &formatted, "<stdin>"),
            report,
        ),
    }
}

fn finish_stdout(stdout: &mut impl Write, text: &str, code: ExitCode) -> ExitCode {
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Ok(()) => code,
        Err(e) => {
            stdout_error(e);
            ExitCode::from(2)
        }
    }
}

/// Report a failed stdout write. A closed pipe (`brief fmt --diff | head`)
/// is the reader saying it has seen enough, not an error worth a message.
fn stdout_error(e: std::io::Error) {
    if e.kind() != std::io::ErrorKind::BrokenPipe {
        eprintln!("brief: cannot write to stdout: {}", e);
    }
}

/// Expand the command-line paths into the `.brf` files to format, in
/// order and without duplicates. Every path is checked before anything is
/// formatted, so a typo never leaves a run half done.
fn collect_targets(paths: &[PathBuf]) -> Result<Vec<Target>, String> {
    let mut found: Vec<Target> = Vec::new();
    if paths.is_empty() {
        let cwd =
            std::env::current_dir().map_err(|e| format!("cannot read current directory: {}", e))?;
        // Only the root matters here, so a `brief.toml` that fails to load
        // must not stop formatting.
        let root = brief::project::discover_root(&cwd).unwrap_or_else(|| cwd.clone());
        for path in brief::project::brf_files(&root) {
            let shown = path
                .strip_prefix(&cwd)
                .unwrap_or(&path)
                .display()
                .to_string();
            found.push(Target { path, shown });
        }
    }
    for arg in paths {
        let meta =
            std::fs::metadata(arg).map_err(|e| format!("cannot read {}: {}", arg.display(), e))?;
        if meta.is_dir() {
            for path in brief::project::brf_files(arg) {
                let shown = path.display().to_string();
                found.push(Target { path, shown });
            }
        } else if arg.extension().and_then(|s| s.to_str()) == Some("brf") {
            found.push(Target {
                path: arg.clone(),
                shown: arg.display().to_string(),
            });
        } else {
            return Err(format!(
                "{} is not a .brf file; fmt only formats Brief sources",
                arg.display()
            ));
        }
    }
    let mut seen = HashSet::new();
    found.retain(|t| seen.insert(t.path.canonicalize().unwrap_or_else(|_| t.path.clone())));
    Ok(found)
}

/// Replace `path` with `contents` by writing a temp file beside it and
/// renaming it over the original, so an interrupted run never leaves a
/// half-written source. The temp file takes the original's permissions,
/// and a symlink is followed so the link itself survives.
fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    let target = path.canonicalize()?;
    let perms = std::fs::metadata(&target)?.permissions();
    let dir = target.parent().unwrap_or(Path::new("."));
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let mut attempt = 0u32;
    let (tmp, mut file) = loop {
        let tmp = dir.join(format!(
            ".{}.{}-{}.fmt.tmp",
            name,
            std::process::id(),
            attempt
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 16 => {
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    };
    let result = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::set_permissions(&tmp, perms))
        .and_then(|()| std::fs::rename(&tmp, &target));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn unified_diff(old: &str, new: &str, shown: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .header(&format!("a/{}", shown), &format!("b/{}", shown))
        .to_string()
}

fn summary(mode: Mode, changed: usize, unchanged: usize, failed: usize) -> String {
    let files = |n: usize| format!("{} file{}", n, if n == 1 { "" } else { "s" });
    let (did, did_not) = match mode {
        Mode::InPlace => ("reformatted", "unchanged"),
        _ => ("would be reformatted", "already formatted"),
    };
    let mut line = format!(
        "{} {}, {} {}",
        files(changed),
        did,
        files(unchanged),
        did_not
    );
    if failed > 0 {
        line.push_str(&format!(", {} failed", files(failed)));
    }
    line
}
