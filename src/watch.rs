//! `brief watch` — file/directory watcher that recompiles on change.
//!
//! Behavior (v0.3.7):
//!   - .brf change       → recompile that file
//!   - brief.toml change → recompile all files (default), with one refinement:
//!                         if the only field that changed across the diff is
//!                         a shortcode `template_html`/`template_llm` string,
//!                         only files referencing those shortcodes recompile.
//!   - 100ms debounce per path (notify-debouncer-mini).
//!   - Whole-file recompile only. Block-level incremental is v0.4+.
//!
//! The watcher reuses the existing lex→parse→resolve→validate→emit pipeline.
//! It writes outputs next to each source: `note.brf` → `note.html` /
//! `note.llm.txt` / `note.json`.

use crate::config::{self, Config};
use crate::diag::{Severity, render_all};
use crate::emit::{html, llm};
use crate::lexer;
use crate::parser;
use crate::resolve;
use crate::shortcode::Registry;
use crate::span::SourceMap;
use crate::validate;

use notify_debouncer_mini::{
    DebounceEventResult, DebouncedEvent, new_debouncer, notify::RecursiveMode,
};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

pub const DEBOUNCE_MS: u64 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Html,
    Llm,
    Json,
}

impl Target {
    pub fn parse(s: &str) -> Option<Target> {
        match s {
            "html" => Some(Target::Html),
            "llm" => Some(Target::Llm),
            "json" => Some(Target::Json),
            _ => None,
        }
    }

    pub fn out_ext(self) -> &'static str {
        match self {
            Target::Html => "html",
            Target::Llm => "llm.txt",
            Target::Json => "json",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LlmOpts {
    pub strip_emphasis: bool,
    pub keep_table_rule: bool,
    pub keep_asset_urls: bool,
    pub keep_metadata: bool,
}

#[derive(Clone, Debug)]
pub struct WatchOpts {
    pub paths: Vec<PathBuf>,
    pub target: Target,
    pub config_path: PathBuf,
    pub llm_opts: LlmOpts,
}

#[derive(Debug)]
pub enum CompileOutcome {
    Ok {
        src: PathBuf,
        dst: PathBuf,
        diag_count: usize,
    },
    LexError {
        src: PathBuf,
    },
    Errors {
        src: PathBuf,
        count: usize,
    },
    IoError {
        src: PathBuf,
        msg: String,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConfigDelta {
    /// Recompile every tracked file.
    All,
    /// Only template strings of these shortcodes differ; recompile files
    /// that reference any of them.
    Templates(BTreeSet<String>),
    /// No effective change.
    None,
}

pub struct Engine {
    pub config_path: PathBuf,
    pub config: Config,
    pub registry: Registry,
    pub target: Target,
    pub llm_opts: LlmOpts,
    pub files: BTreeSet<PathBuf>,
    /// Per-file: set of shortcode names referenced in source. Built/refreshed
    /// each time a file is compiled.
    shortcode_use: HashMap<PathBuf, HashSet<String>>,
}

impl Engine {
    pub fn load(opts: &WatchOpts) -> Result<Self, String> {
        let config = if opts.config_path.exists() {
            config::load(&opts.config_path).map_err(|e| format!("bad config: {e}"))?
        } else {
            Config::default()
        };
        let registry = config::registry_from(&config);
        let files: BTreeSet<PathBuf> = discover_brf(&opts.paths).into_iter().collect();
        Ok(Engine {
            config_path: opts.config_path.clone(),
            config,
            registry,
            target: opts.target,
            llm_opts: opts.llm_opts.clone(),
            files,
            shortcode_use: HashMap::new(),
        })
    }

    pub fn add_file(&mut self, p: PathBuf) {
        self.files.insert(p);
    }

    pub fn compile_all<W: Write>(&mut self, log: &mut W) -> Vec<CompileOutcome> {
        let files: Vec<PathBuf> = self.files.iter().cloned().collect();
        files
            .into_iter()
            .map(|f| self.compile_one(&f, log))
            .collect()
    }

    pub fn compile_one<W: Write>(&mut self, path: &Path, log: &mut W) -> CompileOutcome {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                let _ = writeln!(log, "brief: cannot read {}: {}", path.display(), e);
                return CompileOutcome::IoError {
                    src: path.to_path_buf(),
                    msg: e.to_string(),
                };
            }
        };
        let source = raw.strip_prefix('\u{feff}').unwrap_or(&raw).to_string();

        // Refresh shortcode-usage cache before compile so a brief.toml-only
        // template change can correctly filter to files using the shortcode.
        self.shortcode_use
            .insert(path.to_path_buf(), scan_shortcode_uses(&source));

        let src = SourceMap::new(path.to_string_lossy(), source);
        let opts = validate::ValidateOpts {
            strict_heading_levels: self.config.compile.strict_heading_levels,
        };

        let tokens = match lexer::lex(&src) {
            Ok(t) => t,
            Err(d) => {
                let _ = write!(log, "{}", render_all(&d, &src));
                return CompileOutcome::LexError {
                    src: path.to_path_buf(),
                };
            }
        };
        let (mut doc, mut diags) = parser::parse(tokens, &src);
        diags.extend(resolve::resolve(&mut doc, &self.registry));
        diags.extend(validate::validate(&doc, &opts, &src));
        let errors = diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        if errors > 0 {
            let _ = write!(log, "{}", render_all(&diags, &src));
            return CompileOutcome::Errors {
                src: path.to_path_buf(),
                count: errors,
            };
        }
        if !diags.is_empty() {
            let _ = write!(log, "{}", render_all(&diags, &src));
        }

        let output = match self.target {
            Target::Html => html::render(&doc, &self.registry),
            Target::Llm => {
                let lopts = llm::Opts {
                    strip_emphasis: self.llm_opts.strip_emphasis,
                    keep_table_rule: self.llm_opts.keep_table_rule,
                    keep_asset_urls: self.llm_opts.keep_asset_urls,
                    keep_metadata: self.llm_opts.keep_metadata,
                    minify_code_blocks: self.config.compile.llm.minify_code_blocks,
                    minify_languages: self.config.compile.llm.minify_languages.clone(),
                    preserve_code_fences: self.config.compile.llm.preserve_code_fences,
                };
                let (out, warnings) = llm::render(&doc, &self.registry, &lopts);
                for w in &warnings {
                    let _ = writeln!(log, "brief: {}", w);
                }
                out
            }
            Target::Json => format!("{:#?}\n", doc),
        };

        let dst = output_path(path, self.target);
        if let Err(e) = std::fs::write(&dst, &output) {
            let _ = writeln!(log, "brief: cannot write {}: {}", dst.display(), e);
            return CompileOutcome::IoError {
                src: path.to_path_buf(),
                msg: e.to_string(),
            };
        }
        CompileOutcome::Ok {
            src: path.to_path_buf(),
            dst,
            diag_count: diags.len(),
        }
    }

    pub fn reload_config(&mut self) -> Result<ConfigDelta, String> {
        let new_cfg = if self.config_path.exists() {
            config::load(&self.config_path).map_err(|e| e.to_string())?
        } else {
            Config::default()
        };
        let delta = diff_config(&self.config, &new_cfg);
        self.config = new_cfg;
        self.registry = config::registry_from(&self.config);
        Ok(delta)
    }

    pub fn files_using(&self, shortcodes: &BTreeSet<String>) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|f| {
                self.shortcode_use
                    .get(*f)
                    .map(|uses| uses.iter().any(|u| shortcodes.contains(u)))
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }
}

pub fn output_path(src: &Path, target: Target) -> PathBuf {
    let ext = target.out_ext();
    let mut p = src.to_path_buf();
    if p.extension().and_then(|s| s.to_str()) == Some("brf") {
        p.set_extension(ext);
    } else {
        let mut name = p.file_name().unwrap_or_default().to_os_string();
        name.push(".");
        name.push(ext);
        p.set_file_name(name);
    }
    p
}

/// Scan a Brief source for shortcode invocations. Returns the set of
/// names referenced. Conservative: catches `@name(...)`, `@name[...]`, and
/// bare `@name` followed by whitespace. Used only to decide which files
/// need recompilation when a template changes — false positives just cause
/// a redundant recompile, never a missed one.
pub fn scan_shortcode_uses(source: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let bytes = source.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'@' {
            i += 1;
            continue;
        }
        // Escaped `\@` is not a shortcode start.
        if i > 0 && bytes[i - 1] == b'\\' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < bytes.len() {
            let c = bytes[j];
            if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' {
                j += 1;
            } else {
                break;
            }
        }
        if j > start {
            // Heuristic: must be followed by `(`, `[`, whitespace, or EOF.
            // Filters out things like email addresses (`a@b.com` — no `@`
            // prefix at start; conservative anyway since bytes[i] == '@'
            // and previous char is `a`, but `a` is alphanumeric and we'd
            // otherwise add `b` — so check: prior char must not be ident).
            let prev_is_ident = i > 0
                && (bytes[i - 1].is_ascii_alphanumeric()
                    || bytes[i - 1] == b'_'
                    || bytes[i - 1] == b'-');
            let next = bytes.get(j).copied().unwrap_or(b' ');
            let next_ok =
                matches!(next, b'(' | b'[' | b' ' | b'\t' | b'\n' | b'\r') || j == bytes.len();
            if !prev_is_ident && next_ok {
                if let Ok(name) = std::str::from_utf8(&bytes[start..j]) {
                    out.insert(name.to_string());
                }
            }
        }
        i = j.max(i + 1);
    }
    out
}

pub fn diff_config(old: &Config, new: &Config) -> ConfigDelta {
    if old.project != new.project || old.compile != new.compile || old.hooks != new.hooks {
        return ConfigDelta::All;
    }

    let old_keys: BTreeSet<&String> = old.shortcodes.keys().collect();
    let new_keys: BTreeSet<&String> = new.shortcodes.keys().collect();
    if old_keys != new_keys {
        // Adding/removing a shortcode is a structural change — could affect
        // any file (e.g. a previously unknown shortcode is now resolved).
        return ConfigDelta::All;
    }

    let mut template_changed: BTreeSet<String> = BTreeSet::new();
    for k in &new_keys {
        let o = &old.shortcodes[*k];
        let n = &new.shortcodes[*k];
        if o.kind != n.kind || o.arguments != n.arguments {
            return ConfigDelta::All;
        }
        if o.template_html != n.template_html || o.template_llm != n.template_llm {
            template_changed.insert((*k).clone());
        }
    }

    if template_changed.is_empty() {
        ConfigDelta::None
    } else {
        ConfigDelta::Templates(template_changed)
    }
}

fn discover_brf(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for p in paths {
        if p.is_file() {
            if p.extension().and_then(|s| s.to_str()) == Some("brf") {
                out.push(canonicalize_or_clone(p));
            }
        } else if p.is_dir() {
            walk_dir(p, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            // Skip hidden/build dirs to avoid recursing into `.git`, `target`, etc.
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            walk_dir(&path, out);
        } else if ft.is_file() && path.extension().and_then(|s| s.to_str()) == Some("brf") {
            out.push(canonicalize_or_clone(&path));
        }
    }
}

fn canonicalize_or_clone(p: &Path) -> PathBuf {
    if let Ok(canon) = p.canonicalize() {
        return canon;
    }
    // Path may not exist (e.g. just deleted). Canonicalizing the parent
    // and re-joining the filename gives us a stable identity for lookup
    // against `engine.files` (whose entries were canonicalized at
    // discovery time, before any deletion).
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name()) {
        if !parent.as_os_str().is_empty() {
            if let Ok(parent_canon) = parent.canonicalize() {
                return parent_canon.join(name);
            }
        }
    }
    p.to_path_buf()
}

pub fn run(opts: WatchOpts) -> Result<(), String> {
    let mut log = std::io::stderr();
    let mut engine = Engine::load(&opts)?;

    if engine.files.is_empty() {
        let _ = writeln!(log, "brief: no .brf files found in given paths");
    }

    // Initial compile pass.
    let outcomes = engine.compile_all(&mut log);
    print_outcomes(&outcomes, &mut log);

    let (tx, rx) = channel();
    let mut debouncer = new_debouncer(
        Duration::from_millis(DEBOUNCE_MS),
        move |res: DebounceEventResult| {
            let _ = tx.send(res);
        },
    )
    .map_err(|e| format!("watcher init failed: {e}"))?;

    // Watch each input path. For directories, recursively. For files, watch
    // the parent (some platforms don't report file-only watches reliably).
    let mut watched: HashSet<PathBuf> = HashSet::new();
    for p in &opts.paths {
        let canon = canonicalize_or_clone(p);
        let (target, mode) = if canon.is_file() {
            (
                canon
                    .parent()
                    .map(|pp| pp.to_path_buf())
                    .unwrap_or_else(|| PathBuf::from(".")),
                RecursiveMode::NonRecursive,
            )
        } else {
            (canon.clone(), RecursiveMode::Recursive)
        };
        if watched.insert(target.clone()) {
            debouncer
                .watcher()
                .watch(&target, mode)
                .map_err(|e| format!("watch {}: {e}", target.display()))?;
        }
    }

    // Always also watch the config file's containing directory.
    let cfg_dir = engine
        .config_path
        .parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let cfg_dir = canonicalize_or_clone(&cfg_dir);
    if watched.insert(cfg_dir.clone()) {
        debouncer
            .watcher()
            .watch(&cfg_dir, RecursiveMode::NonRecursive)
            .map_err(|e| format!("watch {}: {e}", cfg_dir.display()))?;
    }

    let _ = writeln!(
        log,
        "brief: watching {} file{} (debounce {}ms; Ctrl-C to exit)",
        engine.files.len(),
        if engine.files.len() == 1 { "" } else { "s" },
        DEBOUNCE_MS
    );

    while let Ok(res) = rx.recv() {
        match res {
            Ok(events) => handle_events(events, &mut engine, &mut log),
            Err(e) => {
                let _ = writeln!(log, "brief: watcher error: {}", e);
            }
        }
    }
    Ok(())
}

fn handle_events<W: Write>(events: Vec<DebouncedEvent>, engine: &mut Engine, log: &mut W) {
    let paths: Vec<PathBuf> = events.into_iter().map(|e| e.path).collect();
    handle_change_paths(&paths, engine, log);
}

/// Dispatch a list of changed paths through the engine. Public for tests
/// (so we don't have to drive the FS watcher to exercise the logic).
pub fn handle_change_paths<W: Write>(paths: &[PathBuf], engine: &mut Engine, log: &mut W) {
    let cfg_canon = canonicalize_or_clone(&engine.config_path);
    let mut config_changed = false;
    let mut brf_changes: BTreeSet<PathBuf> = BTreeSet::new();

    for path in paths {
        let p = canonicalize_or_clone(path);
        if p == cfg_canon {
            config_changed = true;
            continue;
        }
        if p.extension().and_then(|s| s.to_str()) == Some("brf") {
            // New file? Track it. (Discovery is one-shot; this lets a new
            // file dropped into the watched dir start being compiled.)
            if !engine.files.contains(&p) && p.exists() {
                engine.add_file(p.clone());
            }
            if engine.files.contains(&p) {
                brf_changes.insert(p);
            }
        }
    }

    if config_changed {
        match engine.reload_config() {
            Ok(ConfigDelta::All) => {
                let _ = writeln!(log, "brief: config changed → recompiling all");
                let outcomes = engine.compile_all(log);
                print_outcomes(&outcomes, log);
                return; // covers any concurrent .brf changes
            }
            Ok(ConfigDelta::Templates(names)) => {
                let listed: Vec<String> = names.iter().cloned().collect();
                let _ = writeln!(
                    log,
                    "brief: template change ({}) → recompiling files using {}",
                    listed.join(", "),
                    listed.join(", ")
                );
                let files = engine.files_using(&names);
                if files.is_empty() {
                    let _ = writeln!(
                        log,
                        "brief: (no tracked file references {})",
                        listed.join(", ")
                    );
                }
                for f in &files {
                    let outcome = engine.compile_one(f, log);
                    print_outcomes(std::slice::from_ref(&outcome), log);
                }
                // Fall through to also handle direct .brf changes.
            }
            Ok(ConfigDelta::None) => { /* nothing material in config */ }
            Err(e) => {
                let _ = writeln!(log, "brief: config reload failed: {}", e);
            }
        }
    }

    for f in brf_changes {
        if !f.exists() {
            engine.files.remove(&f);
            engine.shortcode_use.remove(&f);
            let _ = writeln!(log, "brief: {} removed (no longer tracked)", f.display());
            continue;
        }
        let outcome = engine.compile_one(&f, log);
        print_outcomes(std::slice::from_ref(&outcome), log);
    }
}

fn print_outcomes<W: Write>(outcomes: &[CompileOutcome], log: &mut W) {
    for o in outcomes {
        match o {
            CompileOutcome::Ok {
                src,
                dst,
                diag_count,
            } => {
                let suffix = if *diag_count > 0 {
                    format!(
                        " ({} note{})",
                        diag_count,
                        if *diag_count == 1 { "" } else { "s" }
                    )
                } else {
                    String::new()
                };
                let _ = writeln!(
                    log,
                    "brief: {} → {}{}",
                    src.display(),
                    dst.display(),
                    suffix
                );
            }
            CompileOutcome::LexError { src } => {
                let _ = writeln!(log, "brief: {} → FAILED (lex error)", src.display());
            }
            CompileOutcome::Errors { src, count } => {
                let _ = writeln!(
                    log,
                    "brief: {} → FAILED ({} error{})",
                    src.display(),
                    count,
                    if *count == 1 { "" } else { "s" }
                );
            }
            CompileOutcome::IoError { src, msg } => {
                let _ = writeln!(log, "brief: {} → FAILED: {}", src.display(), msg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcode::{ArgSpec, ArgType, ShortKindOpt, Shortcode};

    fn cfg_with_shortcode(name: &str, tpl_html: Option<&str>, tpl_llm: Option<&str>) -> Config {
        let mut c = Config::default();
        c.shortcodes.insert(
            name.into(),
            Shortcode {
                kind: ShortKindOpt::Inline,
                arguments: Default::default(),
                template_html: tpl_html.map(str::to_string),
                template_llm: tpl_llm.map(str::to_string),
            },
        );
        c
    }

    #[test]
    fn diff_config_no_change() {
        let a = Config::default();
        let b = Config::default();
        assert_eq!(diff_config(&a, &b), ConfigDelta::None);
    }

    #[test]
    fn diff_config_template_only_change() {
        let a = cfg_with_shortcode("note", Some("<div>{{content}}</div>"), None);
        let b = cfg_with_shortcode("note", Some("<aside>{{content}}</aside>"), None);
        let mut expected = BTreeSet::new();
        expected.insert("note".to_string());
        assert_eq!(diff_config(&a, &b), ConfigDelta::Templates(expected));
    }

    #[test]
    fn diff_config_kind_change_is_structural() {
        let a = cfg_with_shortcode("note", Some("x"), None);
        let mut b = cfg_with_shortcode("note", Some("x"), None);
        b.shortcodes.get_mut("note").unwrap().kind = ShortKindOpt::Block;
        assert_eq!(diff_config(&a, &b), ConfigDelta::All);
    }

    #[test]
    fn diff_config_arguments_change_is_structural() {
        let mut a = Config::default();
        a.shortcodes.insert(
            "note".into(),
            Shortcode {
                kind: ShortKindOpt::Inline,
                arguments: Default::default(),
                template_html: Some("x".into()),
                template_llm: None,
            },
        );
        let mut b = a.clone();
        b.shortcodes.get_mut("note").unwrap().arguments.insert(
            "kind".into(),
            ArgSpec {
                ty: ArgType::String,
                required: false,
                position: None,
                oneof: None,
            },
        );
        assert_eq!(diff_config(&a, &b), ConfigDelta::All);
    }

    #[test]
    fn diff_config_added_shortcode_is_structural() {
        let a = Config::default();
        let b = cfg_with_shortcode("note", Some("x"), None);
        assert_eq!(diff_config(&a, &b), ConfigDelta::All);
    }

    #[test]
    fn diff_config_compile_change_is_structural() {
        let mut a = Config::default();
        let mut b = Config::default();
        b.compile.strict_heading_levels = !a.compile.strict_heading_levels;
        assert_eq!(diff_config(&a, &b), ConfigDelta::All);
        a.compile.strict_heading_levels = b.compile.strict_heading_levels;
        assert_eq!(diff_config(&a, &b), ConfigDelta::None);
    }

    #[test]
    fn scan_picks_up_block_and_inline_shortcodes() {
        let s = "# Title\n\n@note(kind: tip)\nbody\n@end\n\nSome @link(\"u\") text.\n";
        let uses = scan_shortcode_uses(s);
        assert!(uses.contains("note"), "uses={:?}", uses);
        assert!(uses.contains("link"), "uses={:?}", uses);
        assert!(uses.contains("end"), "uses={:?}", uses); // benign false positive
    }

    #[test]
    fn scan_ignores_email_addresses() {
        let s = "Email me at foo@example.com please.\n";
        let uses = scan_shortcode_uses(s);
        // `@example` should not be picked up because previous char is `o`,
        // an identifier char.
        assert!(!uses.contains("example"), "uses={:?}", uses);
    }

    #[test]
    fn scan_ignores_escaped_at() {
        let s = "literal \\@notashort here\n";
        let uses = scan_shortcode_uses(s);
        assert!(!uses.contains("notashort"), "uses={:?}", uses);
    }

    #[test]
    fn output_path_replaces_brf_extension() {
        let p = output_path(Path::new("/tmp/foo/note.brf"), Target::Html);
        assert_eq!(p, PathBuf::from("/tmp/foo/note.html"));
        let p = output_path(Path::new("/tmp/foo/note.brf"), Target::Llm);
        assert_eq!(p, PathBuf::from("/tmp/foo/note.llm.txt"));
        let p = output_path(Path::new("/tmp/foo/note.brf"), Target::Json);
        assert_eq!(p, PathBuf::from("/tmp/foo/note.json"));
    }

    #[test]
    fn output_path_no_brf_extension_appends() {
        let p = output_path(Path::new("/tmp/foo/note"), Target::Html);
        assert_eq!(p, PathBuf::from("/tmp/foo/note.html"));
    }

    #[test]
    fn engine_compile_writes_html_output() {
        let dir = std::env::temp_dir().join("brief-watch-engine-html");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("doc.brf");
        std::fs::write(&f, "# Hello\n").unwrap();
        let opts = WatchOpts {
            paths: vec![dir.clone()],
            target: Target::Html,
            config_path: dir.join("brief.toml"),
            llm_opts: LlmOpts::default(),
        };
        let mut engine = Engine::load(&opts).unwrap();
        let mut sink: Vec<u8> = Vec::new();
        let outcomes = engine.compile_all(&mut sink);
        assert_eq!(outcomes.len(), 1);
        match &outcomes[0] {
            CompileOutcome::Ok { dst, .. } => {
                let html = std::fs::read_to_string(dst).unwrap();
                assert!(html.contains("<h1"), "html={}", html);
                assert!(html.contains("Hello"), "html={}", html);
            }
            other => panic!("expected Ok, got {:?}", other),
        }
    }

    #[test]
    fn engine_files_using_shortcode_filters_correctly() {
        let dir = std::env::temp_dir().join("brief-watch-files-using");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("uses_note.brf");
        let b = dir.join("plain.brf");
        std::fs::write(&a, "@note(kind: tip)\nhi\n@end\n").unwrap();
        std::fs::write(&b, "# plain\n").unwrap();

        // Config with a `note` block shortcode so resolution succeeds.
        let cfg_path = dir.join("brief.toml");
        std::fs::write(
            &cfg_path,
            r#"
[shortcodes.note]
kind = "block"
template_html = "<aside>{{content}}</aside>"
"#,
        )
        .unwrap();

        let opts = WatchOpts {
            paths: vec![dir.clone()],
            target: Target::Html,
            config_path: cfg_path,
            llm_opts: LlmOpts::default(),
        };
        let mut engine = Engine::load(&opts).unwrap();
        let mut sink: Vec<u8> = Vec::new();
        let _ = engine.compile_all(&mut sink);

        let mut names = BTreeSet::new();
        names.insert("note".to_string());
        let users = engine.files_using(&names);
        assert_eq!(users.len(), 1);
        assert!(users[0].ends_with("uses_note.brf"), "users={:?}", users);
    }
}
