use brief::config;
use brief::diag::render_all;
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
    },
    Explain {
        code: String,
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
        } => run_compile(
            input,
            target,
            cfg,
            report_tokens,
            strip_emphasis,
            keep_table_rule,
            keep_asset_urls,
        ),
        Cmd::Explain { code } => run_explain(&code),
    }
}

fn run_compile(
    input: PathBuf,
    target: String,
    cfg_path: Option<PathBuf>,
    report_tokens: bool,
    strip_emphasis: bool,
    keep_table_rule: bool,
    keep_asset_urls: bool,
) -> ExitCode {
    let source = match std::fs::read_to_string(&input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("brief: cannot read {}: {}", input.display(), e);
            return ExitCode::from(2);
        }
    };
    let source = source
        .strip_prefix('\u{feff}')
        .unwrap_or(&source)
        .to_string();
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
    diags.extend(validate::validate(&doc, &opts));
    if !diags.is_empty() {
        eprint!("{}", render_all(&diags, &src));
        return ExitCode::from(1);
    }

    let output = match target.as_str() {
        "html" => html::render(&doc, &registry),
        "llm" => {
            let lopts = llm::Opts {
                strip_emphasis,
                keep_table_rule,
                keep_asset_urls,
            };
            llm::render(&doc, &registry, &lopts)
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
            "Shortcodes must be registered in `brief.toml` under `[shortcodes.<name>]` (or be a built-in: link, image, kbd, t, code, callout, math, footnote).",
        ),
        (
            Code::TabCharacter,
            "Tabs are forbidden in Brief sources. Configure your editor to insert two spaces.",
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
