use brief::config;
use brief::diag::{Severity, render_all};
use brief::emit::{html, llm};
use brief::lexer;
use brief::parser;
use brief::resolve;
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
        #[arg(long, default_value = "html")]
        target: String,
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
    },
    Explain {
        code: String,
    },
    Convert {
        /// One or more Markdown input files.
        inputs: Vec<PathBuf>,
        /// Override output path. Single-input only.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Write Brief to stdout instead of a file. Single-input only.
        #[arg(long)]
        stdout: bool,
        /// Overwrite existing destination files.
        #[arg(long)]
        force: bool,
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
        #[arg(long, default_value = "html")]
        target: String,
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
        ),
        Cmd::Explain { code } => run_explain(&code),
        Cmd::Convert {
            inputs,
            output,
            stdout,
            force,
        } => run_convert(inputs, output, stdout, force),
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
    target: String,
    cfg_path: Option<PathBuf>,
    strip_emphasis: bool,
    keep_table_rule: bool,
    keep_asset_urls: bool,
    keep_metadata: bool,
    no_clear: bool,
) -> ExitCode {
    use brief::watch::{LlmOpts, Target as WatchTarget, WatchOpts};
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
    let config_path = cfg_path.unwrap_or_else(|| PathBuf::from("brief.toml"));
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
    target: String,
    cfg_path: Option<PathBuf>,
    report_tokens: bool,
    strip_emphasis: bool,
    keep_table_rule: bool,
    keep_asset_urls: bool,
    keep_metadata: bool,
    convert: bool,
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
    let opts = validate::ValidateOpts {
        strict_heading_levels: cfg.compile.strict_heading_levels,
    };

    let tokens = match lexer::lex(&src) {
        Ok(t) => t,
        Err(d) => {
            eprint!("{}", render_all(&d, &src));
            return ExitCode::from(1);
        }
    };
    let (mut doc, mut diags) = parser::parse(tokens, &src);
    diags.extend(resolve::resolve(&mut doc, &registry));
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

    print!("{}", output);

    if report_tokens && target == "llm" {
        let chars = output.chars().count();
        let approx = (chars + 3) / 4;
        eprintln!(
            "brief: {} -> {} chars, ~{} tokens (cl100k estimate)",
            src.path, chars, approx
        );
    }
    ExitCode::SUCCESS
}

fn run_explain(code: &str) -> ExitCode {
    use brief::diag::Code;
    let table: &[(Code, &str)] = &[
        (
            Code::HeadingTooDeep,
            "Brief supports six heading levels. `#######` and deeper are errors. Restructure the document or split it.",
        ),
        (
            Code::OrderedListSequence,
            "Ordered lists must number 1, 2, 3, ... renumbering by the renderer is forbidden. Either fix the source or convert to an unordered list.",
        ),
        (
            Code::EmphasisSameMarker,
            "`*outer *inner* outer*` is ambiguous. Use a different marker for the inner span: `*outer _inner_ outer*`.",
        ),
        (
            Code::TableColumnMismatch,
            "Every row in a `@t` table must have the same number of cells as the header row. Add or remove cells until they match.",
        ),
        (
            Code::UnknownShortcode,
            "Shortcodes must be registered in `brief.toml` under `[shortcodes.<name>]` (or be a built-in: link, image, kbd, sub, sup, details, t, code, callout, math, footnote). Note: `@br` is intentionally not a shortcode — use `\\` at end of line for a hard break.",
        ),
        (
            Code::TabCharacter,
            "Tabs are forbidden in Brief sources. Configure your editor to insert two spaces.",
        ),
        (
            Code::UnterminatedFrontmatter,
            "A frontmatter block opened with `+++` was never closed. Add a closing `+++` line, or remove the opening if the document has no metadata.",
        ),
        (
            Code::FrontmatterToml,
            "Frontmatter content must be valid TOML. Brief deliberately uses TOML (not YAML) to match `brief.toml`. Fix the TOML syntax in the `+++ ... +++` block.",
        ),
        (
            Code::UnknownCodeAttribute,
            "Code-fence attributes are `@`-prefixed identifiers after the language tag (e.g. ```json @nominify). v0.3 recognizes `@nominify`, `@minify`, and `@minify-keep-comments`. Anything else is a compile error so typos are caught early.",
        ),
        (
            Code::ConflictingCodeAttributes,
            "`@nominify` and `@minify` (or `@minify-keep-comments`) are mutually exclusive: one says \"never minify this block\" and the other says \"always minify this block.\" Drop one.",
        ),
        (
            Code::CodeBlockLineCount,
            "A code block is being minified to a single (or near-single) line, but the original spanned more than 50 lines. After minification the LLM consumer cannot reference the original line numbers. Either accept this (silence with `@nominify`) or split the block into smaller pieces.",
        ),
        (
            Code::LineCommentConverted,
            "`@minify-keep-comments` converts `//` line comments into `/* */` block form so they can survive on a single minified line. If the comment body contains `*/` the conversion will break the comment; audit those blocks. Or use `@nominify` to keep the source verbatim.",
        ),
        (
            Code::RefusedLanguage,
            "Python, YAML, and Makefile use significant whitespace; minification cannot be performed safely without parsing the language. Such blocks are emitted verbatim and the LLM consumer pays full cost. Drop the `@minify` attribute or remove the language from `compile.llm.minify_languages`.",
        ),
    ];
    for (c, text) in table {
        if c.as_str() == code {
            println!("error[{}]: {}\n\n{}", c.as_str(), c.message(), text);
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
        let src = match std::fs::read_to_string(input) {
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
        let result = brief::convert::convert(&src, &input.to_string_lossy());

        let dest = if use_stdout {
            None
        } else if let Some(o) = &output {
            Some(o.clone())
        } else {
            Some(default_output_path(input))
        };

        match dest {
            None => {
                print!("{}", result.brief_source);
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
                if let Err(e) = std::fs::write(&path, &result.brief_source) {
                    eprintln!(
                        "brief: {} → FAILED: cannot write {}: {}",
                        input.display(),
                        path.display(),
                        e
                    );
                    failed += 1;
                    continue;
                }
                let n = result.diagnostics.len();
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

        for d in &result.diagnostics {
            eprintln!(
                "  note[{}]: {}:{}:{}: {}",
                d.hole.slug(),
                input.display(),
                d.line,
                d.col,
                d.note
            );
        }
        total_holes += result.diagnostics.len();
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
    if input.extension().and_then(|s| s.to_str()) == Some("md") {
        input.with_extension("brf")
    } else {
        let mut p = input.to_path_buf();
        let mut name = p.file_name().unwrap_or_default().to_os_string();
        name.push(".brf");
        p.set_file_name(name);
        p
    }
}
