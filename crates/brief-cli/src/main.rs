// mimalloc roughly doubles parse/emit throughput on allocation-heavy
// documents (the AST is built from many small owned strings and vecs).
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use brief::config;
use brief::diag::{Severity, render_all};
use brief::emit::{html, llm};
use brief::lexer;
use brief::parser;
use brief::shortcode::Registry;
use brief::span::SourceMap;
use brief::validate;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "brief", version, about = "Brief language compiler")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Compile {
        input: PathBuf,
        /// Output target: html, llm, or json. Defaults to
        /// `compile.default_target` from brief.toml, then html.
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        report_tokens: bool,
        #[arg(long)]
        strip_emphasis: bool,
        #[arg(long)]
        keep_table_rule: bool,
        #[arg(long)]
        keep_asset_urls: bool,
        #[arg(long)]
        keep_metadata: bool,
        /// Treat the input as Markdown and convert to Brief in memory before
        /// compiling. Equivalent to `brief convert ... | brief compile -` but
        /// in one step.
        #[arg(long)]
        convert: bool,
        /// Write output to a file derived from the input path
        /// (`doc.brf` → `doc.html` / `doc.txt` / `doc.json`).
        /// Mutually exclusive with `--output`.
        #[arg(short = 'w', long, conflicts_with = "output")]
        write: bool,
        /// Write output to PATH. Mutually exclusive with `--write`.
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,
    },
    Explain {
        code: String,
    },
    /// Convert between Markdown and Brief. Direction is chosen by
    /// extension: `.md` inputs convert to Brief, `.brf` inputs convert to
    /// Markdown.
    Convert {
        /// One or more input files (`.md` → Brief, `.brf` → Markdown).
        inputs: Vec<PathBuf>,
        /// Override output path. Single-input only.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Write converted output to stdout instead of a file.
        /// Single-input only.
        #[arg(long)]
        stdout: bool,
        /// Overwrite existing destination files.
        #[arg(long)]
        force: bool,
        /// Disable the post-convert self-test. By default, `brief convert`
        /// checks its own output: converted Brief must compile, and
        /// converted Markdown must survive a round-trip back through the
        /// Markdown→Brief converter. Pass `--no-strict` to write the
        /// (possibly broken) output regardless.
        #[arg(long)]
        no_strict: bool,
    },
    /// Reformat Brief source(s) according to the canonical style.
    /// gofmt-doctrine: no config beyond an explicit opt-in for the
    /// controversial frontmatter sort.
    Fmt {
        /// One or more `.brf` inputs. With no inputs, reads from stdin and
        /// writes formatted output to stdout.
        inputs: Vec<PathBuf>,
        /// Exit non-zero (and list affected paths to stderr) if any input
        /// would be changed by formatting. Mutually exclusive with `--write`.
        #[arg(long, conflicts_with = "write")]
        check: bool,
        /// Rewrite each input in place. Mutually exclusive with `--check`.
        #[arg(long)]
        write: bool,
        /// Sort top-level frontmatter keys alphabetically. Off by default
        /// — re-emitting the TOML loses comment positions.
        #[arg(long)]
        sort_frontmatter: bool,
    },
    /// Watch files/dirs and recompile on change. 100ms debounce. Whole-file
    /// recompile only.
    Watch {
        /// Files or directories to watch. Defaults to the current directory.
        paths: Vec<PathBuf>,
        /// Output target: html, llm, or json. Defaults to
        /// `compile.default_target` from brief.toml, then html.
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        strip_emphasis: bool,
        #[arg(long)]
        keep_table_rule: bool,
        #[arg(long)]
        keep_asset_urls: bool,
        #[arg(long)]
        keep_metadata: bool,
        /// Do not print a clear-screen sequence between recompile runs.
        #[arg(long)]
        no_clear: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Compile {
            input,
            target,
            config: cfg,
            report_tokens,
            strip_emphasis,
            keep_table_rule,
            keep_asset_urls,
            keep_metadata,
            convert,
            write,
            output,
        } => run_compile(
            input,
            target,
            cfg,
            report_tokens,
            strip_emphasis,
            keep_table_rule,
            keep_asset_urls,
            keep_metadata,
            convert,
            write,
            output,
        ),
        Cmd::Explain { code } => run_explain(&code),
        Cmd::Convert {
            inputs,
            output,
            stdout,
            force,
            no_strict,
        } => run_convert(inputs, output, stdout, force, no_strict),
        Cmd::Fmt {
            inputs,
            check,
            write,
            sort_frontmatter,
        } => run_fmt(inputs, check, write, sort_frontmatter),
        Cmd::Watch {
            paths,
            target,
            config: cfg,
            strip_emphasis,
            keep_table_rule,
            keep_asset_urls,
            keep_metadata,
            no_clear,
        } => run_watch(
            paths,
            target,
            cfg,
            strip_emphasis,
            keep_table_rule,
            keep_asset_urls,
            keep_metadata,
            no_clear,
        ),
    }
}

fn run_watch(
    paths: Vec<PathBuf>,
    target: Option<String>,
    cfg_path: Option<PathBuf>,
    strip_emphasis: bool,
    keep_table_rule: bool,
    keep_asset_urls: bool,
    keep_metadata: bool,
    no_clear: bool,
) -> ExitCode {
    use brief::watch::{LlmOpts, Target as WatchTarget, WatchOpts};
    let config_path = cfg_path.unwrap_or_else(|| PathBuf::from("brief.toml"));
    let target = match target {
        Some(t) => t,
        None => config::load(&config_path)
            .ok()
            .and_then(|c| c.compile.default_target)
            .unwrap_or_else(|| "html".to_string()),
    };
    let target = match WatchTarget::parse(&target) {
        Some(t) => t,
        None => {
            eprintln!(
                "brief: unknown target `{}` (expected html, llm, json)",
                target
            );
            return ExitCode::from(2);
        }
    };
    let paths = if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths
    };
    let opts = WatchOpts {
        paths,
        target,
        config_path,
        llm_opts: LlmOpts {
            strip_emphasis,
            keep_table_rule,
            keep_asset_urls,
            keep_metadata,
        },
        no_clear,
    };
    if let Err(e) = brief::watch::run(opts) {
        eprintln!("brief: {}", e);
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn run_compile(
    input: PathBuf,
    target: Option<String>,
    cfg_path: Option<PathBuf>,
    report_tokens: bool,
    strip_emphasis: bool,
    keep_table_rule: bool,
    keep_asset_urls: bool,
    keep_metadata: bool,
    convert: bool,
    write: bool,
    output_path: Option<PathBuf>,
) -> ExitCode {
    let raw = match std::fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("brief: cannot read {}: {}", input.display(), e);
            return ExitCode::from(2);
        }
    };
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string();

    let is_markdown = input.extension().and_then(|s| s.to_str()) == Some("md");

    let source = if convert {
        let result = brief::convert::convert(&raw, &input.to_string_lossy());
        for d in &result.diagnostics {
            eprintln!(
                "brief: note[{}]: {}:{}:{}: {}",
                d.hole.slug(),
                input.display(),
                d.line,
                d.col,
                d.note
            );
        }
        result.brief_source
    } else if is_markdown {
        eprintln!(
            "brief: {} looks like Markdown — pass `--convert` to convert and compile in one step",
            input.display()
        );
        return ExitCode::from(2);
    } else {
        raw
    };

    let src = SourceMap::new(input.to_string_lossy(), source);

    let cfg = match cfg_path {
        Some(p) => match config::load(&p) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("brief: bad config {}: {}", p.display(), e);
                return ExitCode::from(2);
            }
        },
        None => {
            let candidate = std::path::Path::new("brief.toml");
            if candidate.exists() {
                match config::load(candidate) {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("brief: bad brief.toml: {}", e);
                        return ExitCode::from(2);
                    }
                }
            } else {
                config::Config::default()
            }
        }
    };
    let registry: Registry = config::registry_from(&cfg);
    let target = target
        .or_else(|| cfg.compile.default_target.clone())
        .unwrap_or_else(|| "html".to_string());
    let opts = validate::ValidateOpts {
        strict_heading_levels: cfg.compile.strict_heading_levels,
    };

    let abs_input = input.canonicalize().unwrap_or_else(|_| input.clone());
    let project = match discover_project(&abs_input) {
        Ok(p) => p,
        Err(()) => return ExitCode::from(1),
    };

    let tokens = match lexer::lex(&src) {
        Ok(t) => t,
        Err(d) => {
            eprint!("{}", render_all(&d, &src));
            return ExitCode::from(1);
        }
    };
    let (mut doc, mut diags) = parser::parse(tokens, &src);
    let resolve_project = project.as_ref().map(|(root, idx)| {
        let rel = abs_input
            .strip_prefix(root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| input.clone());
        (idx, rel)
    });
    let project_ref = resolve_project
        .as_ref()
        .map(|(idx, rel)| brief::resolve::ResolveProject {
            index: idx,
            current: rel.as_path(),
        });
    diags.extend(brief::resolve::resolve_with_project(
        &mut doc,
        &registry,
        project_ref.as_ref(),
    ));
    diags.extend(validate::validate(&doc, &opts, &src));
    let has_errors = diags.iter().any(|d| d.severity == Severity::Error);
    if has_errors {
        eprint!("{}", render_all(&diags, &src));
        return ExitCode::from(1);
    } else if !diags.is_empty() {
        eprint!("{}", render_all(&diags, &src));
    }

    let output = match target.as_str() {
        "html" => html::render(&doc, &registry),
        "llm" => {
            let lopts = llm::Opts {
                strip_emphasis,
                keep_table_rule,
                keep_asset_urls,
                keep_metadata,
                minify_code_blocks: cfg.compile.llm.minify_code_blocks,
                minify_languages: cfg.compile.llm.minify_languages.clone(),
                preserve_code_fences: cfg.compile.llm.preserve_code_fences,
            };
            let (out, warnings) = llm::render(&doc, &registry, &lopts);
            for w in &warnings {
                eprintln!("brief: {}", w);
            }
            out
        }
        "json" => format!("{:#?}\n", doc),
        other => {
            eprintln!(
                "brief: unknown target `{}` (expected html, llm, json)",
                other
            );
            return ExitCode::from(2);
        }
    };

    let dst: Option<PathBuf> = if let Some(p) = output_path {
        Some(p)
    } else if write {
        Some(compile_output_path(&input, &target))
    } else {
        None
    };

    match &dst {
        None => {
            print!("{}", output);
        }
        Some(path) => {
            // `-w` derives the output extension from the target (llm -> .txt),
            // so compiling a `.txt` input would silently replace the source.
            let same_file = match (std::fs::canonicalize(path), std::fs::canonicalize(&input)) {
                (Ok(a), Ok(b)) => a == b,
                _ => path == &input,
            };
            if same_file {
                eprintln!(
                    "brief: refusing to overwrite input {} with compiled output; use -o with a different path",
                    input.display()
                );
                return ExitCode::from(2);
            }
            if let Err(e) = std::fs::write(path, &output) {
                eprintln!("brief: cannot write {}: {}", path.display(), e);
                return ExitCode::from(2);
            }
            eprintln!("brief: {} -> {}", input.display(), path.display());
        }
    }

    if report_tokens && target == "llm" {
        let chars = output.chars().count();
        let approx = (chars + 3) / 4;
        eprintln!(
            "brief: {} -> {} chars, ~{} tokens (chars/4 heuristic)",
            src.path, chars, approx
        );
    }
    ExitCode::SUCCESS
}

/// Discover the project root above `abs_input` and build its index,
/// surfacing lex/parse errors from the pre-pass. Each set of diagnostics is
/// rendered against its own SourceMap so that line numbers and source
/// excerpts refer to the correct file. `Err(())` means an indexed file
/// failed to lex/parse (already rendered to stderr).
fn discover_project(
    abs_input: &std::path::Path,
) -> Result<Option<(PathBuf, brief::project::ProjectIndex)>, ()> {
    let Some(root) = brief::project::discover_root(abs_input) else {
        return Ok(None);
    };
    let (idx, prepass_diags) = brief::project::build_index(&root);
    let mut has_err = false;
    for fd in &prepass_diags {
        if fd.diagnostics.is_empty() {
            continue;
        }
        if fd.diagnostics.iter().any(|d| d.severity == Severity::Error) {
            has_err = true;
        }
        eprint!("{}", render_all(&fd.diagnostics, &fd.source));
    }
    if has_err {
        Err(())
    } else {
        Ok(Some((root, idx)))
    }
}

fn compile_output_path(input: &std::path::Path, target: &str) -> PathBuf {
    let ext = match target {
        "html" => "html",
        "llm" => "txt",
        "json" => "json",
        _ => "out",
    };
    input.with_extension(ext)
}

fn run_explain(code: &str) -> ExitCode {
    use brief::diag::Code;
    // Long-form text lives in brief-core (`Code::explain`) so the CLI and
    // the LSP hover can never drift apart.
    for c in Code::ALL {
        if c.as_str() == code {
            println!("error[{}]: {}\n\n{}", c.as_str(), c.message(), c.explain());
            return ExitCode::SUCCESS;
        }
    }
    eprintln!("brief: unknown error code `{}`", code);
    ExitCode::from(2)
}

fn run_convert(
    inputs: Vec<PathBuf>,
    output: Option<PathBuf>,
    use_stdout: bool,
    force: bool,
    no_strict: bool,
) -> ExitCode {
    if inputs.is_empty() {
        eprintln!("brief: convert requires at least one input file");
        return ExitCode::from(2);
    }
    if inputs.len() > 1 && output.is_some() {
        eprintln!("brief: -o cannot be used with multiple inputs");
        return ExitCode::from(2);
    }
    if inputs.len() > 1 && use_stdout {
        eprintln!("brief: --stdout cannot be used with multiple inputs");
        return ExitCode::from(2);
    }
    if use_stdout && output.is_some() {
        eprintln!("brief: --stdout and -o are mutually exclusive");
        return ExitCode::from(2);
    }

    let mut failed: usize = 0;
    let mut total_holes: usize = 0;
    let multi = inputs.len() > 1;

    for input in &inputs {
        let raw = match std::fs::read_to_string(input) {
            Ok(s) => s,
            Err(e) => {
                eprintln!(
                    "brief: {} → FAILED: cannot read input: {}",
                    input.display(),
                    e
                );
                failed += 1;
                continue;
            }
        };

        // Direction is chosen by extension: `.brf` exports to Markdown,
        // anything else migrates to Brief.
        let is_reverse = input.extension().and_then(|s| s.to_str()) == Some("brf");
        let (converted, notes) = if is_reverse {
            match convert_reverse(input, &raw, no_strict) {
                Ok(v) => v,
                Err(()) => {
                    failed += 1;
                    continue;
                }
            }
        } else {
            let result = brief::convert::convert(&raw, &input.to_string_lossy());

            // Strict-by-default: pipe through the lex + parse self-test.
            // Skipped on --no-strict.
            if !no_strict {
                if let Err(rendered) =
                    strict_self_test(&result.brief_source, &input.to_string_lossy())
                {
                    eprintln!(
                        "brief: {} → FAILED: --strict self-test rejected the converted output:",
                        input.display()
                    );
                    eprint!("{}", rendered);
                    eprintln!(
                        "brief: pass --no-strict to write the broken output anyway, or fix the converter."
                    );
                    failed += 1;
                    continue;
                }
            }
            let notes = result
                .diagnostics
                .iter()
                .map(|d| {
                    format!(
                        "  note[{}]: {}:{}:{}: {}",
                        d.hole.slug(),
                        input.display(),
                        d.line,
                        d.col,
                        d.note
                    )
                })
                .collect();
            (result.brief_source, notes)
        };

        let dest = if use_stdout {
            None
        } else if let Some(o) = &output {
            Some(o.clone())
        } else {
            Some(default_output_path(input))
        };

        match dest {
            None => {
                print!("{}", converted);
            }
            Some(path) => {
                if path.exists() && !force {
                    eprintln!(
                        "brief: {} → FAILED: output {} exists (use --force to overwrite)",
                        input.display(),
                        path.display()
                    );
                    failed += 1;
                    continue;
                }
                if let Err(e) = std::fs::write(&path, &converted) {
                    eprintln!(
                        "brief: {} → FAILED: cannot write {}: {}",
                        input.display(),
                        path.display(),
                        e
                    );
                    failed += 1;
                    continue;
                }
                let n = notes.len();
                if n == 0 {
                    eprintln!("brief: {} → {} (clean)", input.display(), path.display());
                } else {
                    eprintln!(
                        "brief: {} → {} ({} hole{})",
                        input.display(),
                        path.display(),
                        n,
                        if n == 1 { "" } else { "s" }
                    );
                }
            }
        }

        for note in &notes {
            eprintln!("{}", note);
        }
        total_holes += notes.len();
    }

    if multi {
        eprintln!(
            "brief: {} of {} files converted, {} hole{} flagged total",
            inputs.len() - failed,
            inputs.len(),
            total_holes,
            if total_holes == 1 { "" } else { "s" }
        );
    }

    if failed > 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn run_fmt(inputs: Vec<PathBuf>, check: bool, write: bool, sort_frontmatter: bool) -> ExitCode {
    use brief::fmt;
    use std::io::Read;

    let opts = fmt::Opts { sort_frontmatter };

    if inputs.is_empty() {
        // Stdin → stdout. `--write` is meaningless here; `--check` is honored.
        if write {
            eprintln!("brief: --write requires at least one input file");
            return ExitCode::from(2);
        }
        let mut buf = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
            eprintln!("brief: cannot read stdin: {}", e);
            return ExitCode::from(2);
        }
        let formatted = fmt::format(&buf, &opts);
        if check {
            if formatted != buf {
                eprintln!("brief: <stdin> would be reformatted");
                return ExitCode::from(1);
            }
            return ExitCode::SUCCESS;
        }
        print!("{}", formatted);
        return ExitCode::SUCCESS;
    }

    let multi = inputs.len() > 1;
    let mut errors: usize = 0;
    let mut would_change: usize = 0;

    for input in &inputs {
        let raw = match std::fs::read_to_string(input) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("brief: cannot read {}: {}", input.display(), e);
                errors += 1;
                continue;
            }
        };
        let formatted = fmt::format(&raw, &opts);
        if check {
            if formatted != raw {
                // Print a unified diff to stdout (matching gofmt's convention).
                let diff = similar::TextDiff::from_lines(&raw, &formatted);
                let mut udiff = diff.unified_diff();
                udiff.header(
                    &format!("{} (original)", input.display()),
                    &format!("{} (formatted)", input.display()),
                );
                print!("{}", udiff);
                would_change += 1;
            }
        } else if write {
            if formatted == raw {
                continue;
            }
            if let Err(e) = std::fs::write(input, &formatted) {
                eprintln!("brief: cannot write {}: {}", input.display(), e);
                errors += 1;
            }
        } else {
            // Default mode: print to stdout. With multiple inputs that
            // would interleave; refuse it the way gofmt does.
            if multi {
                eprintln!(
                    "brief: refusing to print multiple files to stdout; pass --write or --check"
                );
                return ExitCode::from(2);
            }
            print!("{}", formatted);
        }
    }

    if errors > 0 {
        return ExitCode::from(2);
    }
    if check && would_change > 0 {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn default_output_path(input: &std::path::Path) -> PathBuf {
    match input.extension().and_then(|s| s.to_str()) {
        Some("md") => input.with_extension("brf"),
        Some("brf") => input.with_extension("md"),
        _ => {
            let mut p = input.to_path_buf();
            let mut name = p.file_name().unwrap_or_default().to_os_string();
            name.push(".brf");
            p.set_file_name(name);
            p
        }
    }
}

/// Convert one `.brf` input to Markdown.
///
/// Runs the full compile front-end (lex → parse → resolve → validate) so
/// `@ref` targets and shortcode arguments obey the same rules as
/// `brief compile`, then emits Markdown. Prints failure details to stderr
/// and returns `Err(())` when the source does not compile or the strict
/// round-trip self-test rejects the output.
fn convert_reverse(
    input: &std::path::Path,
    raw: &str,
    no_strict: bool,
) -> Result<(String, Vec<String>), ()> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let src = SourceMap::new(input.to_string_lossy(), raw.to_string());

    // Same config fallback as `run_compile` with no `--config`: a brief.toml
    // in the working directory, else defaults. Custom shortcodes need their
    // `template_html` for the Markdown fallback expansion.
    let cfg = {
        let candidate = std::path::Path::new("brief.toml");
        if candidate.exists() {
            match config::load(candidate) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("brief: {} → FAILED: bad brief.toml: {}", input.display(), e);
                    return Err(());
                }
            }
        } else {
            config::Config::default()
        }
    };
    let registry = config::registry_from(&cfg);

    let abs_input = input.canonicalize().unwrap_or_else(|_| input.to_path_buf());
    let project = match discover_project(&abs_input) {
        Ok(p) => p,
        Err(()) => {
            eprintln!(
                "brief: {} → FAILED: project pre-pass reported errors",
                input.display()
            );
            return Err(());
        }
    };

    let tokens = match lexer::lex(&src) {
        Ok(t) => t,
        Err(d) => {
            eprintln!("brief: {} → FAILED:", input.display());
            eprint!("{}", render_all(&d, &src));
            return Err(());
        }
    };
    let (mut doc, mut diags) = parser::parse(tokens, &src);
    let resolve_project = project.as_ref().map(|(root, idx)| {
        let rel = abs_input
            .strip_prefix(root)
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|_| input.to_path_buf());
        (idx, rel)
    });
    let project_ref = resolve_project
        .as_ref()
        .map(|(idx, rel)| brief::resolve::ResolveProject {
            index: idx,
            current: rel.as_path(),
        });
    diags.extend(brief::resolve::resolve_with_project(
        &mut doc,
        &registry,
        project_ref.as_ref(),
    ));
    let opts = validate::ValidateOpts {
        strict_heading_levels: cfg.compile.strict_heading_levels,
    };
    diags.extend(validate::validate(&doc, &opts, &src));
    if diags.iter().any(|d| d.severity == Severity::Error) {
        eprintln!("brief: {} → FAILED:", input.display());
        eprint!("{}", render_all(&diags, &src));
        return Err(());
    } else if !diags.is_empty() {
        eprint!("{}", render_all(&diags, &src));
    }

    let result = brief::convert::to_markdown(&doc, &registry, &src);

    // Strict-by-default: the emitted Markdown must survive the forward
    // converter and compile back to valid Brief. Skipped on --no-strict.
    if !no_strict {
        let back = brief::convert::convert(&result.markdown, &input.to_string_lossy());
        if let Err(rendered) = strict_self_test(&back.brief_source, &input.to_string_lossy()) {
            eprintln!(
                "brief: {} → FAILED: --strict round-trip self-test rejected the converted Markdown:",
                input.display()
            );
            eprint!("{}", rendered);
            eprintln!("brief: pass --no-strict to write the output anyway, or fix the converter.");
            return Err(());
        }
    }

    let notes = result
        .diagnostics
        .iter()
        .map(|d| {
            format!(
                "  note[{}]: {}:{}:{}: {}",
                d.hole.slug(),
                input.display(),
                d.line,
                d.col,
                d.note
            )
        })
        .collect();
    Ok((result.markdown, notes))
}

/// Result of running the strict self-test against converted Brief.
/// `Ok(())` if the source compiles cleanly under the lex+parse pipeline;
/// `Err(rendered_diagnostics)` otherwise.
fn strict_self_test(brief_source: &str, name: &str) -> Result<(), String> {
    let src = SourceMap::new(name.to_string(), brief_source.to_string());
    let diags = match lexer::lex(&src) {
        Ok(toks) => parser::parse(toks, &src).1,
        Err(d) => d,
    };
    let any_error = diags.iter().any(|d| d.severity == Severity::Error);
    if any_error {
        Err(render_all(&diags, &src))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod strict_tests {
    use super::*;

    #[test]
    fn rejects_brief_that_does_not_compile() {
        // A header row with three cells and a data row with one — Brief
        // emits `B0502 TableColumnMismatch`. Hand-crafting the broken
        // Brief sidesteps the entire converter so the test only measures
        // the self-test contract.
        let broken = "@t\n| A | B | C\n| only one\n";
        let err = strict_self_test(broken, "t.brf").expect_err("must reject");
        assert!(err.contains("B0502"), "{}", err);
    }

    #[test]
    fn accepts_clean_brief() {
        let clean = "# Title\n\nA paragraph.\n";
        strict_self_test(clean, "t.brf").expect("must accept");
    }
}
