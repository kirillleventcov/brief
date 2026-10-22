//! `brief watch` — file/directory watcher that recompiles on change.
//!
//! Behavior:
//! - `.brf` change: recompile that file, unless its output was already
//!   built from exactly this content.
//! - `brief.toml` change: recompile the files that project governs, with
//!   one refinement: if the only field that changed across the diff is a
//!   shortcode `template_html`/`template_llm` string, only files
//!   referencing those shortcodes recompile.
//! - 100ms debounce: a batch fires once events have been quiet that long.
//! - Whole-file recompile only. Block-level incremental is v0.4+.
//!
//! Every file compiles in its own project (`Project::discover`: the nearest
//! `brief.toml` above it, `--config` replacing its config) through the same
//! front-end as `brief compile`, so watch and compile never disagree. It
//! writes outputs next to each source: `note.brf` → `note.html` /
//! `note.txt` / `note.json`.

use crate::config::Config;
use crate::diag::Severity;
use crate::emit::{html, llm};
use crate::project::{self, IndexBuild, Project};
use crate::span::SourceMap;

use notify::event::{AccessKind, AccessMode, ModifyKind};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Stderr, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

/// How long, in milliseconds, events must stay quiet before a batch of
/// changes is handled.
pub const DEBOUNCE_MS: u64 = 100;

/// An output format `brief watch` can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Target {
    Html,
    Llm,
    Json,
}

impl Target {
    /// File extension of this target's output, without the dot.
    #[must_use]
    pub fn extension(self) -> &'static str {
        match self {
            Target::Html => "html",
            Target::Llm => "txt",
            Target::Json => "json",
        }
    }
}

impl FromStr for Target {
    type Err = ParseTargetError;

    /// Parse a target name as given to `--target` or `compile.default_target`.
    fn from_str(s: &str) -> Result<Target, ParseTargetError> {
        match s {
            "html" => Ok(Target::Html),
            "llm" => Ok(Target::Llm),
            "json" => Ok(Target::Json),
            _ => Err(ParseTargetError),
        }
    }
}

/// A string named no known `Target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseTargetError;

impl fmt::Display for ParseTargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown target (expected html, llm, json)")
    }
}

impl Error for ParseTargetError {}

/// Command-line options for the LLM target; `brief.toml` supplies the rest.
#[derive(Clone, Debug, Default)]
pub struct LlmOptions {
    /// Drop the markers around emphasized text, keeping only the text.
    pub strip_emphasis: bool,
    /// Emit the `|---|` separator row under a table's header row.
    pub keep_table_rule: bool,
    /// Include the `src` of `@image` next to its alt text.
    pub keep_asset_urls: bool,
    /// Re-emit the document's `+++` frontmatter at the top of the output.
    pub keep_metadata: bool,
}

/// Everything `run` needs: what to watch and how to compile it.
#[derive(Clone, Debug, Default)]
pub struct WatchOptions {
    /// Files and directories to watch; directories are searched for `.brf`
    /// files recursively.
    pub paths: Vec<PathBuf>,
    /// Explicit `--target`. `None` uses each file's project
    /// `compile.default_target`, then html, as `brief compile` does.
    pub target: Option<Target>,
    /// `--config`: replaces every project's `brief.toml`. The root, and so
    /// `@ref` resolution, still comes from where each file lives.
    pub config_override: Option<PathBuf>,
    /// Options used whenever a file compiles to the LLM target.
    pub llm_options: LlmOptions,
    /// When true, suppress the clear-screen sequence between recompile runs.
    pub no_clear: bool,
}

/// The result of compiling one tracked file.
#[derive(Debug)]
#[non_exhaustive]
pub enum CompileOutcome {
    /// The output at `dst` was written; `diagnostic_count` warnings were
    /// reported along the way.
    Ok {
        src: PathBuf,
        dst: PathBuf,
        diagnostic_count: usize,
    },
    /// The source did not lex; nothing was written.
    LexError { src: PathBuf },
    /// The source has `count` errors; nothing was written.
    Errors { src: PathBuf, count: usize },
    /// Reading the source or writing the output failed.
    IoError { src: PathBuf, message: String },
    /// The file's `brief.toml` (or `--config`) does not load, or names an
    /// unknown default target.
    BadConfig { src: PathBuf, message: String },
}

/// How a config edit affects the files it governs.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigDelta {
    /// Recompile every tracked file.
    All,
    /// Only template strings of these shortcodes differ; recompile files
    /// that reference any of them.
    Templates(BTreeSet<String>),
    /// No effective change.
    None,
}

/// What a file's current output was built from, so a later event can tell
/// whether anything that matters has changed.
struct Built {
    /// Content hash of the source behind the output on disk. `None` after a
    /// failed compile, so saving the file again always retries it.
    hash: Option<u64>,
    root: Option<PathBuf>,
    config_path: Option<PathBuf>,
    config: Config,
}

/// The watch state: the tracked files and what each output was built
/// from. `run` drives it from file-system events; tests drive it directly.
pub struct Engine {
    /// `--config`: replaces every project's `brief.toml`.
    pub config_override: Option<PathBuf>,
    /// Explicit `--target`, or `None` for each project's default.
    pub target: Option<Target>,
    /// Options used whenever a file compiles to the LLM target.
    pub llm_options: LlmOptions,
    /// Canonical paths of every tracked `.brf` file.
    pub files: BTreeSet<PathBuf>,
    /// When true, suppress the clear-screen sequence between recompile runs.
    pub no_clear: bool,
    /// Canonical watch inputs; a new `.brf` is tracked only inside these.
    scope: Vec<PathBuf>,
    /// Per-file: set of shortcode names referenced in source. Built/refreshed
    /// each time a file is compiled.
    shortcode_use: HashMap<PathBuf, HashSet<String>>,
    built: HashMap<PathBuf, Built>,
    /// Every output this engine has written. Their change events are our
    /// own doing and never trigger a rebuild.
    outputs: HashSet<PathBuf>,
}

impl Engine {
    /// Track every `.brf` under `options.paths`.
    ///
    /// # Errors
    ///
    /// A config that fails to load is a startup error, the same verdict
    /// `brief compile` gives.
    pub fn load(options: &WatchOptions) -> Result<Self, String> {
        let files: BTreeSet<PathBuf> = discover_brf(&options.paths).into_iter().collect();
        let mut seen: HashSet<Option<PathBuf>> = HashSet::new();
        for f in &files {
            if seen.insert(project::discover_root(f)) {
                Project::discover(f, options.config_override.as_deref())?;
            }
        }
        Ok(Engine {
            config_override: options.config_override.clone(),
            target: options.target,
            llm_options: options.llm_options.clone(),
            files,
            no_clear: options.no_clear,
            scope: options
                .paths
                .iter()
                .map(|p| canonicalize_or_clone(p))
                .collect(),
            shortcode_use: HashMap::new(),
            built: HashMap::new(),
            outputs: HashSet::new(),
        })
    }

    /// Track `p` from now on.
    pub fn add_file(&mut self, p: PathBuf) {
        self.files.insert(p);
    }

    /// Compile every tracked file.
    pub fn compile_all<W: Write>(&mut self, log: &mut W) -> Vec<CompileOutcome> {
        let files: Vec<PathBuf> = self.files.iter().cloned().collect();
        self.compile_files(&files, log)
    }

    /// Compile the single file at `path`, writing diagnostics to `log`.
    pub fn compile_one<W: Write>(&mut self, path: &Path, log: &mut W) -> CompileOutcome {
        self.compile_in(path, &mut HashMap::new(), log)
    }

    /// Compile `files`, indexing each project tree once for the batch
    /// rather than once per file.
    pub fn compile_files<W: Write>(
        &mut self,
        files: &[PathBuf],
        log: &mut W,
    ) -> Vec<CompileOutcome> {
        let mut indexes = HashMap::new();
        files
            .iter()
            .map(|f| self.compile_in(f, &mut indexes, log))
            .collect()
    }

    fn compile_in<W: Write>(
        &mut self,
        path: &Path,
        indexes: &mut HashMap<PathBuf, IndexBuild>,
        log: &mut W,
    ) -> CompileOutcome {
        let src_path = path.to_path_buf();
        let source = match project::read_source(path) {
            Ok(s) => s,
            Err(e) => {
                let _ = writeln!(log, "brief: cannot read {}: {}", path.display(), e);
                return CompileOutcome::IoError {
                    src: src_path,
                    message: e.to_string(),
                };
            }
        };
        let hash = content_hash(&source);

        // Refresh shortcode-usage cache before compile so a brief.toml-only
        // template change can correctly filter to files using the shortcode.
        self.shortcode_use
            .insert(src_path.clone(), scan_shortcode_uses(&source));

        let project = match Project::discover(path, self.config_override.as_deref()) {
            Ok(p) => p,
            Err(message) => {
                self.built.remove(path);
                return CompileOutcome::BadConfig {
                    src: src_path,
                    message,
                };
            }
        };
        let target = match (self.target, &project.config.compile.default_target) {
            (Some(t), _) => t,
            (None, None) => Target::Html,
            (None, Some(name)) => match name.parse() {
                Ok(t) => t,
                Err(ParseTargetError) => {
                    self.built.remove(path);
                    return CompileOutcome::BadConfig {
                        src: src_path,
                        message: format!("unknown target `{name}` (expected html, llm, json)"),
                    };
                }
            },
        };
        self.built.insert(
            src_path.clone(),
            Built {
                hash: None,
                root: project.root.clone(),
                config_path: project.config_path.clone(),
                config: project.config.clone(),
            },
        );

        let index_build = project.root.as_ref().map(|root| {
            &*indexes
                .entry(root.clone())
                .or_insert_with(|| project::build_index(root))
        });
        let (index, index_diags) = match index_build {
            Some(build) => (Some(&build.index), build.diagnostics.as_slice()),
            None => (None, &[][..]),
        };
        let src = SourceMap::new(path.to_string_lossy(), source);
        let checked = project.check(&src, index, project.rel_path(path).as_deref());
        let _ = write!(log, "{}", checked.render(&src, index_diags));
        let errors = checked
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let diagnostic_count = checked.diagnostics.len();
        let Some(doc) = checked.doc else {
            return CompileOutcome::LexError { src: src_path };
        };
        if errors > 0 {
            return CompileOutcome::Errors {
                src: src_path,
                count: errors,
            };
        }

        let output = match target {
            Target::Html => html::render(&doc, &project.registry),
            Target::Llm => {
                let config = &project.config.compile.llm;
                let options = llm::Options {
                    strip_emphasis: self.llm_options.strip_emphasis,
                    keep_table_rule: self.llm_options.keep_table_rule,
                    keep_asset_urls: self.llm_options.keep_asset_urls,
                    keep_metadata: self.llm_options.keep_metadata,
                    minify_code_blocks: config.minify_code_blocks,
                    minify_languages: config.minify_languages.clone(),
                    preserve_code_fences: config.preserve_code_fences,
                };
                let rendered = llm::render(&doc, &project.registry, &options);
                for w in &rendered.warnings {
                    let _ = writeln!(log, "brief: {}", w);
                }
                rendered.text
            }
            Target::Json => format!("{:#?}\n", doc),
        };

        let dst = output_path(path, target);
        if let Err(e) = fs::write(&dst, &output) {
            let _ = writeln!(log, "brief: cannot write {}: {}", dst.display(), e);
            return CompileOutcome::IoError {
                src: src_path,
                message: e.to_string(),
            };
        }
        self.outputs.insert(canonicalize_or_clone(&dst));
        if let Some(b) = self.built.get_mut(path) {
            b.hash = Some(hash);
        }
        CompileOutcome::Ok {
            src: src_path,
            dst,
            diagnostic_count,
        }
    }

    /// The tracked files that reference any of `shortcodes`, as of their
    /// last compile.
    #[must_use]
    pub fn files_using(&self, shortcodes: &BTreeSet<String>) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|f| self.uses_any(f, shortcodes))
            .cloned()
            .collect()
    }

    fn uses_any(&self, file: &Path, shortcodes: &BTreeSet<String>) -> bool {
        self.shortcode_use
            .get(file)
            .is_some_and(|uses| uses.iter().any(|u| shortcodes.contains(u)))
    }

    /// Whether `file` still holds exactly what its current output was
    /// built from. Editors and tools touch files without changing them;
    /// rebuilding on that is wasted work at best and a feedback loop at
    /// worst.
    fn is_unchanged(&self, file: &Path) -> bool {
        match (
            self.built.get(file).and_then(|b| b.hash),
            project::read_source(file),
        ) {
            (Some(hash), Ok(s)) => hash == content_hash(&s),
            _ => false,
        }
    }

    /// How the config governing each of `files` differs from the one its
    /// current output was built with. A file whose project root or config
    /// file moved (a `brief.toml` appeared or vanished) needs a full
    /// recompile. A config that fails to load is an `Err` naming it.
    fn config_deltas(&self, files: &[PathBuf]) -> Vec<(PathBuf, Result<ConfigDelta, String>)> {
        let mut projects: HashMap<Option<PathBuf>, Result<Project, String>> = HashMap::new();
        files
            .iter()
            .map(|f| {
                let project = projects
                    .entry(project::discover_root(f))
                    .or_insert_with(|| Project::discover(f, self.config_override.as_deref()));
                let delta = match project {
                    Err(e) => Err(e.clone()),
                    Ok(new) => Ok(match self.built.get(f) {
                        Some(b) if b.root == new.root && b.config_path == new.config_path => {
                            diff_config(&b.config, &new.config)
                        }
                        _ => ConfigDelta::All,
                    }),
                };
                (f.clone(), delta)
            })
            .collect()
    }

    /// Whether a `.brf` that just appeared lies inside what the user asked
    /// to watch (the extra watches on project roots must not widen it).
    fn in_scope(&self, file: &Path) -> bool {
        self.scope.iter().any(|s| {
            if s.is_dir() {
                file.strip_prefix(s).is_ok_and(|rel| {
                    let dirs: Vec<_> = rel.components().collect();
                    dirs[..dirs.len().saturating_sub(1)]
                        .iter()
                        .all(|c| !skipped_dir(&c.as_os_str().to_string_lossy()))
                })
            } else {
                file == s
            }
        })
    }
}

fn content_hash(source: &str) -> u64 {
    let mut h = DefaultHasher::new();
    source.hash(&mut h);
    h.finish()
}

/// Where `src`'s `target` output goes: next to it, with the `.brf`
/// extension replaced (or the target's extension appended to any other
/// name).
#[must_use]
pub fn output_path(src: &Path, target: Target) -> PathBuf {
    let ext = target.extension();
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
#[must_use]
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
            if !prev_is_ident
                && next_ok
                && let Ok(name) = std::str::from_utf8(&bytes[start..j])
            {
                out.insert(name.to_string());
            }
        }
        i = j.max(i + 1);
    }
    out
}

/// How the edit from `old` to `new` affects the files they govern: a
/// change to anything but shortcode templates is `ConfigDelta::All`.
#[must_use]
pub fn diff_config(old: &Config, new: &Config) -> ConfigDelta {
    if old.project != new.project || old.compile != new.compile {
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
    let entries = match fs::read_dir(dir) {
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
            if skipped_dir(name) {
                continue;
            }
            walk_dir(&path, out);
        } else if ft.is_file() && path.extension().and_then(|s| s.to_str()) == Some("brf") {
            out.push(canonicalize_or_clone(&path));
        }
    }
}

/// Hidden and build directories (`.git`, `target`, …) hold no sources.
fn skipped_dir(name: &str) -> bool {
    name.starts_with('.') || name == "target" || name == "node_modules"
}

fn canonicalize_or_clone(p: &Path) -> PathBuf {
    if let Ok(canon) = p.canonicalize() {
        return canon;
    }
    // Path may not exist (e.g. just deleted). Canonicalizing the parent
    // and re-joining the filename gives us a stable identity for lookup
    // against `engine.files` (whose entries were canonicalized at
    // discovery time, before any deletion).
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name())
        && !parent.as_os_str().is_empty()
        && let Ok(parent_canon) = parent.canonicalize()
    {
        return parent_canon.join(name);
    }
    p.to_path_buf()
}

/// Watch `options.paths` until the event channel closes, recompiling on
/// every change and logging to stderr.
///
/// # Errors
///
/// Returns a message when a config fails to load at startup or the file
/// system watcher cannot be set up.
pub fn run(options: WatchOptions) -> Result<(), String> {
    let mut log = io::stderr();
    let mut engine = Engine::load(&options)?;

    if engine.files.is_empty() {
        let _ = writeln!(log, "brief: no .brf files found in given paths");
    }

    // Initial compile pass.
    let outcomes = engine.compile_all(&mut log);
    print_outcomes(&outcomes, &mut log);

    let (tx, rx) = channel::<Result<Vec<PathBuf>, String>>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
        Ok(event) if is_change(&event.kind) => {
            let _ = tx.send(Ok(event.paths));
        }
        Ok(_) => {}
        Err(e) => {
            let _ = tx.send(Err(e.to_string()));
        }
    })
    .map_err(|e| format!("watcher init failed: {e}"))?;

    for (dir, mode) in watch_targets(&options, &engine) {
        watcher
            .watch(&dir, mode)
            .map_err(|e| format!("watch {}: {e}", dir.display()))?;
    }

    let _ = writeln!(
        log,
        "brief: watching {} file{} (debounce {}ms; Ctrl-C to exit)",
        engine.files.len(),
        if engine.files.len() == 1 { "" } else { "s" },
        DEBOUNCE_MS
    );

    // An editor save is several events; wait for a quiet period and handle
    // the batch once.
    let debounce = Duration::from_millis(DEBOUNCE_MS);
    while let Ok(first) = rx.recv() {
        let mut changed: BTreeSet<PathBuf> = BTreeSet::new();
        let mut absorb = |msg: Result<Vec<PathBuf>, String>, log: &mut Stderr| match msg {
            Ok(paths) => changed.extend(paths),
            // Errors tend to be transient (a file vanishing mid-save); a
            // rebuild would not help.
            Err(e) => {
                let _ = writeln!(log, "brief: watcher error: {}", e);
            }
        };
        absorb(first, &mut log);
        loop {
            match rx.recv_timeout(debounce) {
                Ok(msg) => absorb(msg, &mut log),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
        if !changed.is_empty() {
            let paths: Vec<PathBuf> = changed.into_iter().collect();
            handle_change_paths(&paths, &mut engine, &mut log);
        }
    }
    Ok(())
}

/// Directories to watch: each input (a file through its parent, since some
/// platforms report file-only watches unreliably), plus the directories
/// whose `brief.toml` (or `--config` file) governs a tracked file, so a
/// config edit is seen even when it lives above the watched paths.
fn watch_targets(options: &WatchOptions, engine: &Engine) -> Vec<(PathBuf, RecursiveMode)> {
    let mut recursive: Vec<PathBuf> = Vec::new();
    let mut flat: Vec<PathBuf> = Vec::new();
    for p in &options.paths {
        let canon = canonicalize_or_clone(p);
        if canon.is_file() {
            flat.extend(canon.parent().map(Path::to_path_buf));
        } else {
            recursive.push(canon);
        }
    }
    let roots: BTreeSet<PathBuf> = engine
        .files
        .iter()
        .filter_map(|f| project::discover_root(f))
        .collect();
    flat.extend(roots);
    if let Some(cfg) = &engine.config_override {
        flat.extend(canonicalize_or_clone(cfg).parent().map(Path::to_path_buf));
    }

    recursive.sort();
    recursive.dedup();
    flat.sort();
    flat.dedup();
    let mut out: Vec<(PathBuf, RecursiveMode)> = Vec::new();
    for dir in &recursive {
        if !recursive.iter().any(|r| r != dir && dir.starts_with(r)) {
            out.push((dir.clone(), RecursiveMode::Recursive));
        }
    }
    for dir in flat {
        if !recursive.iter().any(|r| dir.starts_with(r)) {
            out.push((dir, RecursiveMode::NonRecursive));
        }
    }
    out
}

/// Whether an event means a watched path's content may have changed.
///
/// notify's inotify backend also reports opens and reads, and every
/// compile opens its source and indexes the rest of the project, so
/// counting access as change makes each rebuild trigger the next one,
/// forever. A close-after-write is the one access event that signals new
/// content; metadata-only changes (`touch`, `chmod`) leave content alone.
fn is_change(kind: &EventKind) -> bool {
    match kind {
        EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any) => true,
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        // Backends report `Any` for what they cannot classify. Missing a
        // real change is worse than a spurious event, which the content
        // hash check turns into a no-op.
        EventKind::Any => true,
        EventKind::Access(_) | EventKind::Modify(_) | EventKind::Other => false,
    }
}

/// Dispatch a list of changed paths through the engine. Public for tests
/// (so we don't have to drive the FS watcher to exercise the logic).
///
/// Events that change nothing (our own outputs, a source saved with the
/// same content, a config edit with no effect) print nothing at all, not
/// even the clear-screen.
pub fn handle_change_paths<W: Write>(paths: &[PathBuf], engine: &mut Engine, log: &mut W) {
    let override_canon = engine.config_override.as_deref().map(canonicalize_or_clone);
    let mut config_all = false;
    let mut config_dirs: Vec<PathBuf> = Vec::new();
    let mut brf_changes: BTreeSet<PathBuf> = BTreeSet::new();
    let mut removed: Vec<PathBuf> = Vec::new();

    for path in paths {
        let p = canonicalize_or_clone(path);
        if engine.outputs.contains(&p) {
            continue;
        }
        if override_canon.as_ref() == Some(&p) {
            config_all = true;
            continue;
        }
        if p.file_name().and_then(|s| s.to_str()) == Some("brief.toml") {
            // A brief.toml appearing, changing or vanishing re-roots or
            // reconfigures every file below it.
            config_dirs.extend(p.parent().map(Path::to_path_buf));
            continue;
        }
        if p.extension().and_then(|s| s.to_str()) != Some("brf") {
            continue;
        }
        if !p.exists() {
            if engine.files.contains(&p) {
                removed.push(p);
            }
            continue;
        }
        // New file? Track it. (Discovery is one-shot; this lets a new file
        // dropped into the watched dir start being compiled.)
        if !engine.files.contains(&p) {
            if !engine.in_scope(&p) {
                continue;
            }
            engine.add_file(p.clone());
        }
        if !engine.is_unchanged(&p) {
            brf_changes.insert(p);
        }
    }

    let mut messages: Vec<String> = Vec::new();
    let mut to_compile: BTreeSet<PathBuf> = BTreeSet::new();
    if config_all || !config_dirs.is_empty() {
        let affected: Vec<PathBuf> = engine
            .files
            .iter()
            .filter(|f| config_all || config_dirs.iter().any(|d| f.starts_with(d)))
            .cloned()
            .collect();
        let mut failures: BTreeSet<String> = BTreeSet::new();
        let mut all = false;
        let mut templates: BTreeSet<String> = BTreeSet::new();
        for (f, delta) in engine.config_deltas(&affected) {
            match delta {
                Err(e) => {
                    failures.insert(e);
                }
                Ok(ConfigDelta::All) => {
                    all = true;
                    to_compile.insert(f);
                }
                Ok(ConfigDelta::Templates(names)) => {
                    if engine.uses_any(&f, &names) {
                        to_compile.insert(f);
                    }
                    templates.extend(names);
                }
                Ok(ConfigDelta::None) => {}
            }
        }
        for e in failures {
            messages.push(format!("brief: config reload failed: {}", e));
        }
        if all {
            messages.push("brief: config changed → recompiling all".to_string());
        } else if !templates.is_empty() {
            let names: Vec<String> = templates.into_iter().collect();
            let listed = names.join(", ");
            messages.push(format!(
                "brief: template change ({}) → recompiling files using {}",
                listed, listed
            ));
            if to_compile.is_empty() {
                messages.push(format!("brief: (no tracked file references {})", listed));
            }
        }
    }

    if messages.is_empty() && removed.is_empty() && brf_changes.is_empty() && to_compile.is_empty()
    {
        return;
    }
    // Clear screen between runs per spec §4.7, unless suppressed.
    if !engine.no_clear {
        let _ = write!(log, "\x1b[2J\x1b[H");
    }
    for m in &messages {
        let _ = writeln!(log, "{}", m);
    }
    for f in removed {
        engine.files.remove(&f);
        engine.shortcode_use.remove(&f);
        engine.built.remove(&f);
        let _ = writeln!(log, "brief: {} removed (no longer tracked)", f.display());
    }
    to_compile.extend(brf_changes);
    let files: Vec<PathBuf> = to_compile.into_iter().collect();
    let outcomes = engine.compile_files(&files, log);
    print_outcomes(&outcomes, log);
}

fn print_outcomes<W: Write>(outcomes: &[CompileOutcome], log: &mut W) {
    for o in outcomes {
        match o {
            CompileOutcome::Ok {
                src,
                dst,
                diagnostic_count,
            } => {
                let suffix = if *diagnostic_count > 0 {
                    format!(
                        " ({} note{})",
                        diagnostic_count,
                        if *diagnostic_count == 1 { "" } else { "s" }
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
            CompileOutcome::IoError { src, message }
            | CompileOutcome::BadConfig { src, message } => {
                let _ = writeln!(log, "brief: {} → FAILED: {}", src.display(), message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shortcode::{ArgSpec, ArgType, Shortcode, ShortcodeKind};

    fn cfg_with_shortcode(name: &str, tpl_html: Option<&str>, tpl_llm: Option<&str>) -> Config {
        let mut c = Config::default();
        c.shortcodes.insert(
            name.into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
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
        b.shortcodes.get_mut("note").unwrap().kind = ShortcodeKind::Block;
        assert_eq!(diff_config(&a, &b), ConfigDelta::All);
    }

    #[test]
    fn diff_config_arguments_change_is_structural() {
        let mut a = Config::default();
        a.shortcodes.insert(
            "note".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
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
    fn reads_are_not_changes_but_writes_are() {
        use notify::event::{CreateKind, DataChange, MetadataKind, RemoveKind, RenameMode};
        let change = [
            EventKind::Create(CreateKind::File),
            EventKind::Remove(RemoveKind::File),
            EventKind::Modify(ModifyKind::Data(DataChange::Content)),
            EventKind::Modify(ModifyKind::Any),
            EventKind::Modify(ModifyKind::Name(RenameMode::To)),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
        ];
        let no_change = [
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::WriteTime)),
        ];
        for k in &change {
            assert!(is_change(k), "{:?} should count", k);
        }
        for k in &no_change {
            assert!(!is_change(k), "{:?} should not count", k);
        }
    }

    #[test]
    fn watch_targets_cover_config_dirs_once() {
        let td = tempfile::TempDir::new().unwrap();
        let root = td.path().canonicalize().unwrap();
        std::fs::write(root.join("brief.toml"), "").unwrap();
        std::fs::create_dir_all(root.join("docs/deep")).unwrap();
        std::fs::write(root.join("docs/a.brf"), "# A\n").unwrap();
        std::fs::write(root.join("docs/deep/b.brf"), "# B\n").unwrap();
        let opts = WatchOptions {
            paths: vec![root.join("docs"), root.join("docs/deep")],
            target: Some(Target::Html),
            config_override: None,
            llm_options: LlmOptions::default(),
            no_clear: true,
        };
        let engine = Engine::load(&opts).unwrap();
        let targets: Vec<(PathBuf, bool)> = watch_targets(&opts, &engine)
            .into_iter()
            .map(|(p, m)| (p, m == RecursiveMode::Recursive))
            .collect();
        // docs/deep is inside docs; the root is watched only for brief.toml.
        assert_eq!(targets, vec![(root.join("docs"), true), (root, false)]);
    }

    #[test]
    fn output_path_replaces_brf_extension() {
        let p = output_path(Path::new("/tmp/foo/note.brf"), Target::Html);
        assert_eq!(p, PathBuf::from("/tmp/foo/note.html"));
        let p = output_path(Path::new("/tmp/foo/note.brf"), Target::Llm);
        assert_eq!(p, PathBuf::from("/tmp/foo/note.txt"));
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
        let opts = WatchOptions {
            paths: vec![dir],
            target: Some(Target::Html),
            config_override: None,
            llm_options: LlmOptions::default(),
            no_clear: true,
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

        let opts = WatchOptions {
            paths: vec![dir],
            target: Some(Target::Html),
            config_override: None,
            llm_options: LlmOptions::default(),
            no_clear: false,
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

    #[test]
    fn clear_screen_emitted_before_subsequent_compile() {
        let dir = std::env::temp_dir().join("brief-watch-clear-emitted");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("doc.brf");
        std::fs::write(&f, "# Hello\n").unwrap();

        let opts = WatchOptions {
            paths: vec![dir],
            target: Some(Target::Html),
            config_override: None,
            llm_options: LlmOptions::default(),
            no_clear: false,
        };
        let mut engine = Engine::load(&opts).unwrap();

        // Simulate initial compile (no clear expected here).
        let mut initial_log: Vec<u8> = Vec::new();
        let _ = engine.compile_all(&mut initial_log);
        assert!(
            !String::from_utf8_lossy(&initial_log).contains("\x1b[2J"),
            "clear should NOT appear on initial compile"
        );

        // Simulate a subsequent file-change event.
        std::fs::write(&f, "# Hello again\n").unwrap();
        let mut log: Vec<u8> = Vec::new();
        handle_change_paths(std::slice::from_ref(&f), &mut engine, &mut log);
        let out = String::from_utf8_lossy(&log);
        assert!(
            out.contains("\x1b[2J\x1b[H"),
            "clear sequence should appear in subsequent compile log; got: {:?}",
            out
        );
    }

    #[test]
    fn clear_screen_suppressed_by_no_clear() {
        let dir = std::env::temp_dir().join("brief-watch-clear-suppressed");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("doc.brf");
        std::fs::write(&f, "# Hello\n").unwrap();

        let opts = WatchOptions {
            paths: vec![dir],
            target: Some(Target::Html),
            config_override: None,
            llm_options: LlmOptions::default(),
            no_clear: true,
        };
        let mut engine = Engine::load(&opts).unwrap();
        let mut initial_log: Vec<u8> = Vec::new();
        let _ = engine.compile_all(&mut initial_log);

        std::fs::write(&f, "# Hello again\n").unwrap();
        let mut log: Vec<u8> = Vec::new();
        handle_change_paths(std::slice::from_ref(&f), &mut engine, &mut log);
        let out = String::from_utf8_lossy(&log);
        assert!(
            out.contains("doc.html"),
            "recompile expected; got: {:?}",
            out
        );
        assert!(
            !out.contains("\x1b[2J"),
            "clear sequence should be suppressed when no_clear=true; got: {:?}",
            out
        );
    }

    #[test]
    fn clear_screen_not_emitted_on_initial_compile() {
        let dir = std::env::temp_dir().join("brief-watch-clear-initial");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("doc.brf");
        std::fs::write(&f, "# Hello\n").unwrap();

        let opts = WatchOptions {
            paths: vec![dir],
            target: Some(Target::Html),
            config_override: None,
            llm_options: LlmOptions::default(),
            no_clear: false,
        };
        let mut engine = Engine::load(&opts).unwrap();
        let mut log: Vec<u8> = Vec::new();
        // This is the initial compile path — no clear-screen should appear.
        let _ = engine.compile_all(&mut log);
        let out = String::from_utf8_lossy(&log);
        assert!(
            !out.contains("\x1b[2J"),
            "clear sequence should NOT appear on initial compile; got: {:?}",
            out
        );
        // Also verify it did compile something.
        let html_path = f.with_extension("html");
        assert!(
            html_path.exists(),
            "html output should exist after initial compile"
        );
    }

    #[test]
    fn watch_engine_runs_project_pre_pass_for_refs() {
        use tempfile::TempDir;

        let td = TempDir::new().unwrap();
        let root = td.path();

        // Without brief.toml: @ref produces B0604.
        std::fs::write(root.join("a.brf"), "# A {#x}\n").unwrap();
        std::fs::write(root.join("b.brf"), "@ref[a.brf#x](X)\n").unwrap();

        let opts = WatchOptions {
            paths: vec![root.to_path_buf()],
            target: Some(Target::Html),
            config_override: None,
            llm_options: LlmOptions::default(),
            no_clear: true,
        };
        let mut engine = Engine::load(&opts).unwrap();
        let mut log = Vec::new();
        let outcome = engine.compile_one(&root.join("b.brf"), &mut log);
        let log_str = String::from_utf8(log).unwrap();
        assert!(
            matches!(outcome, CompileOutcome::Errors { .. }),
            "outcome: {:?}, log: {}",
            outcome,
            log_str,
        );
        assert!(log_str.contains("B0604"), "log: {}", log_str);

        // With brief.toml: clean compile.
        std::fs::write(root.join("brief.toml"), "").unwrap();
        let mut log = Vec::new();
        let outcome = engine.compile_one(&root.join("b.brf"), &mut log);
        let log_str = String::from_utf8(log).unwrap();
        assert!(
            matches!(outcome, CompileOutcome::Ok { .. }),
            "outcome: {:?}, log: {}",
            outcome,
            log_str,
        );
    }
}
