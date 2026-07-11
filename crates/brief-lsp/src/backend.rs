//! LSP backend for the Brief markup language.
//!
//! Position encoding strategy: UTF-16 (the default required by the LSP
//! specification when no alternative is negotiated). All byte offsets coming
//! out of brief-core are converted to UTF-16 code-unit columns via the helper
//! `byte_offset_to_lsp_position`. This avoids any per-client negotiation
//! complexity and is compatible with every editor out of the box.
//!
//! Project awareness: diagnostics run the same pipeline as `brief compile`
//! (see `analysis`). The `brief.toml`-rooted snapshot (config, shortcode
//! registry, `@ref` index) is cached per root and rebuilt on file open and
//! save; open-buffer anchors are overlaid on the disk index on every
//! change so unsaved edits resolve correctly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, ls_types::*};
use tracing::{info, warn};

// The crate is named `brief-core` in Cargo.toml but the Rust crate name
// (lib.name) is `brief` — match that.
use brief::{diag::Severity, fmt, shortcode::ShortKindOpt};

use crate::analysis::{self, Analysis, CompletionCtx, ProjectSnapshot};

// ---------------------------------------------------------------------------
// Span → LSP Range conversion (UTF-16)
// ---------------------------------------------------------------------------

/// Convert a byte offset in `text` into an LSP `Position` using UTF-16
/// code-unit columns (the default LSP encoding).
///
/// The algorithm walks the source up to `byte_offset`, counting:
/// - newlines → advance line, reset column to 0
/// - characters → count UTF-16 code units (BMP = 1, supplementary = 2)
fn byte_offset_to_lsp_position(text: &str, byte_offset: usize) -> Position {
    let clamped = byte_offset.min(text.len());
    let slice = &text[..clamped];

    let mut line: u32 = 0;
    let mut character: u32 = 0;

    for ch in slice.chars() {
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            // UTF-16 code units: BMP plane → 1 unit, supplementary → 2 units.
            let units = if (ch as u32) > 0xFFFF { 2 } else { 1 };
            character += units;
        }
    }

    Position { line, character }
}

/// Convert a brief-core `Span` (byte start + length) into an LSP `Range`.
fn span_to_lsp_range(text: &str, start: u32, len: u32) -> Range {
    let s = start as usize;
    let e = (start + len) as usize;
    let start_pos = byte_offset_to_lsp_position(text, s);
    let end_pos = byte_offset_to_lsp_position(text, e);
    Range {
        start: start_pos,
        end: end_pos,
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Per-open-document state.
#[derive(Debug)]
struct DocState {
    text: String,
    /// Filesystem path (canonicalized when possible); `None` for non-file
    /// URIs (untitled buffers), which are analyzed without project context.
    path: Option<PathBuf>,
    /// Heading anchors of the current buffer content, refreshed on every
    /// change. Overlaid over the disk index so other open documents see
    /// unsaved anchor edits.
    anchors: BTreeSet<String>,
}

/// (buffer text, filesystem path, project snapshot, pipeline result) for
/// one open document — what every request handler starts from.
type AnalyzedDoc = (String, Option<PathBuf>, Option<Arc<ProjectSnapshot>>, Analysis);

#[derive(Debug)]
pub struct Backend {
    client: Client,
    docs: DashMap<Uri, DocState>,
    /// Project snapshots keyed by root (the directory with `brief.toml`).
    snapshots: DashMap<PathBuf, Arc<ProjectSnapshot>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            docs: DashMap::new(),
            snapshots: DashMap::new(),
        }
    }

    fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
        let p = uri.to_file_path()?.into_owned();
        Some(p.canonicalize().unwrap_or(p))
    }

    /// Cached project snapshot for the root above `path`, rebuilding it
    /// when `refresh` is set or nothing is cached yet.
    fn snapshot_for(&self, path: &Path, refresh: bool) -> Option<Arc<ProjectSnapshot>> {
        let root = brief::project::discover_root(path)?;
        if !refresh
            && let Some(s) = self.snapshots.get(&root)
        {
            return Some(s.clone());
        }
        let snap = Arc::new(analysis::load_snapshot(&root));
        self.snapshots.insert(root, snap.clone());
        Some(snap)
    }

    /// Anchor sets of every *other* open buffer under `root`, keyed by
    /// project-relative path.
    fn overlay_for(&self, root: &Path, current: &Uri) -> BTreeMap<String, BTreeSet<String>> {
        let mut overlay = BTreeMap::new();
        for entry in self.docs.iter() {
            if entry.key() == current {
                continue;
            }
            let Some(p) = &entry.value().path else {
                continue;
            };
            if let Some(rel) = analysis::rel_path(root, p) {
                overlay.insert(rel, entry.value().anchors.clone());
            }
        }
        overlay
    }

    /// Run the full compile pipeline over an open document and refresh its
    /// cached anchors. Returns everything a handler needs.
    fn analyze_doc(&self, uri: &Uri, refresh_snapshot: bool) -> Option<AnalyzedDoc> {
        let (text, path) = {
            let state = self.docs.get(uri)?;
            (state.text.clone(), state.path.clone())
        };
        let snapshot = path
            .as_deref()
            .and_then(|p| self.snapshot_for(p, refresh_snapshot));
        let overlay = match &snapshot {
            Some(s) => self.overlay_for(&s.root, uri),
            None => BTreeMap::new(),
        };
        let sourcemap_path = path.clone().unwrap_or_else(|| PathBuf::from(uri.as_str()));
        let analysis = analysis::analyze(&sourcemap_path, &text, snapshot.as_deref(), &overlay);
        if let Some(mut state) = self.docs.get_mut(uri) {
            state.anchors = analysis.anchors.clone();
        }
        Some((text, path, snapshot, analysis))
    }

    /// Analyze and push `textDocument/publishDiagnostics`.
    async fn publish_diagnostics_for(&self, uri: Uri, refresh_snapshot: bool) {
        let Some((text, _, snapshot, analysis)) = self.analyze_doc(&uri, refresh_snapshot) else {
            return;
        };

        let mut lsp_diags: Vec<Diagnostic> = analysis
            .diagnostics
            .iter()
            .map(|d| {
                let range = span_to_lsp_range(&text, d.span.start, d.span.len);
                let code_str = d.code.as_str();
                let message = match &d.label {
                    Some(label) => format!("{}: {} ({})", code_str, label, d.code.message()),
                    None => format!("{}: {}", code_str, d.code.message()),
                };
                let severity = match d.severity {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                };
                Diagnostic {
                    range,
                    severity: Some(severity),
                    code: Some(NumberOrString::String(code_str)),
                    code_description: None,
                    source: Some("brief".to_string()),
                    message,
                    related_information: None,
                    tags: None,
                    data: None,
                }
            })
            .collect();

        // A broken brief.toml is fatal for `brief compile` (exit 2); the
        // editor equivalent is an error pinned to the top of the document.
        if let Some(err) = snapshot.as_deref().and_then(|s| s.config_error.as_deref()) {
            lsp_diags.insert(
                0,
                Diagnostic {
                    range: Range::default(),
                    severity: Some(DiagnosticSeverity::ERROR),
                    code: None,
                    code_description: None,
                    source: Some("brief".to_string()),
                    message: format!("brief.toml failed to load: {err}"),
                    related_information: None,
                    tags: None,
                    data: None,
                },
            );
        }

        self.client.publish_diagnostics(uri, lsp_diags, None).await;
    }

    /// Current text of the file at `path`: the open buffer if one exists,
    /// otherwise the on-disk content.
    fn text_for_path(&self, path: &Path) -> Option<String> {
        for entry in self.docs.iter() {
            if entry.value().path.as_deref() == Some(path) {
                return Some(entry.value().text.clone());
            }
        }
        std::fs::read_to_string(path).ok()
    }

    /// Parse the file at `path` (buffer first, then disk). Best-effort:
    /// parse errors still yield a partial document.
    fn parse_path(&self, path: &Path) -> Option<(String, brief::ast::Document)> {
        let text = self.text_for_path(path)?;
        let src = brief::span::SourceMap::new(path.to_string_lossy(), text.clone());
        let tokens = brief::lexer::lex(&src).ok()?;
        let (doc, _) = brief::parser::parse(tokens, &src);
        Some((text, doc))
    }
}

// ---------------------------------------------------------------------------
// LanguageServer impl — no #[async_trait] (tower-lsp-server v0.21+ uses
// native impl Trait in trait via the rpc! macro)
// ---------------------------------------------------------------------------

impl LanguageServer for Backend {
    // ------------------------------------------------------------------
    // Lifecycle
    // ------------------------------------------------------------------

    async fn initialize(&self, _params: InitializeParams) -> Result<InitializeResult> {
        info!("brief-lsp: initialize");
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "brief-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                // Full sync: we re-parse the whole document on every change.
                // Save notifications drive project-index rebuilds.
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        will_save: None,
                        will_save_wait_until: None,
                        save: Some(TextDocumentSyncSaveOptions::Supported(true)),
                    },
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["@".into(), "[".into(), "#".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "brief-lsp initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        info!("brief-lsp: shutdown");
        Ok(())
    }

    // ------------------------------------------------------------------
    // Document synchronisation
    // ------------------------------------------------------------------

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        info!("brief-lsp: did_open {:?}", uri);
        let path = Self::uri_to_path(&uri);
        self.docs.insert(
            uri.clone(),
            DocState {
                text: params.text_document.text,
                path,
                anchors: BTreeSet::new(),
            },
        );
        // Rebuild the project snapshot on open so the index reflects the
        // state of the world when the user starts editing.
        self.publish_diagnostics_for(uri, true).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // TextDocumentSyncKind::FULL — the single change event contains the
        // complete new text.
        if let Some(change) = params.content_changes.into_iter().last() {
            if let Some(mut state) = self.docs.get_mut(&uri) {
                state.text = change.text;
            }
            self.publish_diagnostics_for(uri, false).await;
        }
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        info!("brief-lsp: did_save {:?}", uri);
        // The saved file may have added/removed anchors or edited
        // brief.toml-adjacent state: rebuild the snapshot once, then
        // refresh every open document under the same root so stale
        // cross-file diagnostics clear without further edits.
        let Some(path) = self.docs.get(&uri).and_then(|s| s.path.clone()) else {
            return;
        };
        let Some(snapshot) = self.snapshot_for(&path, true) else {
            self.publish_diagnostics_for(uri, false).await;
            return;
        };
        let mut to_refresh: Vec<Uri> = Vec::new();
        for entry in self.docs.iter() {
            match &entry.value().path {
                Some(p) if analysis::rel_path(&snapshot.root, p).is_some() => {
                    to_refresh.push(entry.key().clone());
                }
                _ => {}
            }
        }
        for u in to_refresh {
            self.publish_diagnostics_for(u, false).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        info!("brief-lsp: did_close {:?}", uri);
        self.docs.remove(&uri);
        // Clear diagnostics for the closed document.
        self.client.publish_diagnostics(uri, vec![], None).await;
    }

    // ------------------------------------------------------------------
    // Hover
    // ------------------------------------------------------------------

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let Some((text, _, _, analysis)) = self.analyze_doc(uri, false) else {
            warn!("brief-lsp: hover requested for unknown document {:?}", uri);
            return Ok(None);
        };

        // Convert cursor position (UTF-16) to a byte offset so we can check
        // whether it falls inside a diagnostic span.
        let cursor_byte = lsp_position_to_byte_offset(&text, pos);

        // Find the first diagnostic whose span contains the cursor.
        let hit = analysis.diagnostics.iter().find(|d| {
            let s = d.span.start as usize;
            let e = s + d.span.len as usize;
            cursor_byte >= s && cursor_byte <= e
        });

        let hit = match hit {
            Some(h) => h,
            None => return Ok(None),
        };

        let code_str = hit.code.as_str();
        let mut contents = format!("**{}**: {}", code_str, hit.code.message());
        if let Some(label) = &hit.label {
            contents.push_str("\n\n");
            contents.push_str(label);
        }
        contents.push_str("\n\n---\n\n");
        contents.push_str(hit.code.explain());

        if let Some(help) = &hit.help {
            contents.push_str("\n\n**Help:** ");
            contents.push_str(help);
        }

        let range = span_to_lsp_range(&text, hit.span.start, hit.span.len);

        Ok(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: contents,
            }),
            range: Some(range),
        }))
    }

    // ------------------------------------------------------------------
    // Navigation: goto-definition / references for @ref
    // ------------------------------------------------------------------

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let Some((text, _, snapshot, analysis)) = self.analyze_doc(uri, false) else {
            return Ok(None);
        };
        let Some(doc) = &analysis.doc else {
            return Ok(None);
        };
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };

        let cursor_byte = lsp_position_to_byte_offset(&text, pos);
        let Some(site) = analysis::ref_at(doc, cursor_byte) else {
            return Ok(None);
        };
        let (target_path, anchor) = analysis::split_target(&site.target);
        let abs = snapshot.root.join(&target_path);

        let range = match anchor {
            Some(a) => {
                let Some((target_text, target_doc)) = self.parse_path(&abs) else {
                    return Ok(None);
                };
                match analysis::heading_span_for_anchor(&target_doc, &a) {
                    Some(span) => span_to_lsp_range(&target_text, span.start, span.len),
                    None => Range::default(),
                }
            }
            // Whole-file reference: jump to the top.
            None => Range::default(),
        };

        let Some(target_uri) = Uri::from_file_path(&abs) else {
            return Ok(None);
        };
        Ok(Some(GotoDefinitionResponse::Scalar(Location {
            uri: target_uri,
            range,
        })))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let Some((text, path, snapshot, analysis)) = self.analyze_doc(uri, false) else {
            return Ok(None);
        };
        let Some(doc) = &analysis.doc else {
            return Ok(None);
        };
        let Some(snapshot) = snapshot else {
            return Ok(None);
        };

        let cursor_byte = lsp_position_to_byte_offset(&text, pos);

        // The symbol under the cursor: either a @ref (its target) or an
        // anchored heading (all refs pointing at it).
        let (target_path, target_anchor) =
            if let Some(site) = analysis::ref_at(doc, cursor_byte) {
                analysis::split_target(&site.target)
            } else if let Some((_, anchor)) = analysis::heading_anchor_at(doc, cursor_byte) {
                let Some(rel) = path
                    .as_deref()
                    .and_then(|p| analysis::rel_path(&snapshot.root, p))
                else {
                    return Ok(None);
                };
                (rel, Some(anchor))
            } else {
                return Ok(None);
            };

        // Every project file the index knows about, plus any open buffers
        // under the root the disk walk could not see yet.
        let mut candidates: BTreeSet<String> = snapshot.index.anchors.keys().cloned().collect();
        for entry in self.docs.iter() {
            if let Some(p) = &entry.value().path
                && let Some(rel) = analysis::rel_path(&snapshot.root, p)
            {
                candidates.insert(rel);
            }
        }

        let mut locations = Vec::new();
        for rel in candidates {
            let abs = snapshot.root.join(&rel);
            let Some((file_text, file_doc)) = self.parse_path(&abs) else {
                continue;
            };
            let Some(file_uri) = Uri::from_file_path(&abs) else {
                continue;
            };
            for site in analysis::collect_refs(&file_doc) {
                let (p, a) = analysis::split_target(&site.target);
                if p == target_path && a == target_anchor {
                    locations.push(Location {
                        uri: file_uri.clone(),
                        range: span_to_lsp_range(&file_text, site.span.start, site.span.len),
                    });
                }
            }
        }

        if params.context.include_declaration
            && let Some(anchor) = &target_anchor
        {
            let abs = snapshot.root.join(&target_path);
            if let Some((decl_text, decl_doc)) = self.parse_path(&abs)
                && let Some(span) = analysis::heading_span_for_anchor(&decl_doc, anchor)
                && let Some(decl_uri) = Uri::from_file_path(&abs)
            {
                locations.push(Location {
                    uri: decl_uri,
                    range: span_to_lsp_range(&decl_text, span.start, span.len),
                });
            }
        }

        Ok(if locations.is_empty() {
            None
        } else {
            Some(locations)
        })
    }

    // ------------------------------------------------------------------
    // Completion
    // ------------------------------------------------------------------

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let (text, path) = {
            let Some(state) = self.docs.get(uri) else {
                return Ok(None);
            };
            (state.text.clone(), state.path.clone())
        };
        let cursor_byte = lsp_position_to_byte_offset(&text, pos);
        let line_start = text[..cursor_byte].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_prefix = &text[line_start..cursor_byte];

        let Some(ctx) = analysis::completion_context(line_prefix) else {
            return Ok(None);
        };
        let snapshot = path.as_deref().and_then(|p| self.snapshot_for(p, false));

        // Replace the partial word the user already typed.
        let edit_range = |prefix_len: usize| Range {
            start: byte_offset_to_lsp_position(&text, cursor_byte - prefix_len),
            end: byte_offset_to_lsp_position(&text, cursor_byte),
        };
        let item = |name: &str, kind: CompletionItemKind, detail: Option<String>, prefix_len: usize| {
            CompletionItem {
                label: name.to_string(),
                kind: Some(kind),
                detail,
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range: edit_range(prefix_len),
                    new_text: name.to_string(),
                })),
                ..Default::default()
            }
        };

        let items: Vec<CompletionItem> = match ctx {
            CompletionCtx::ShortcodeName { prefix } => {
                let fallback;
                let registry = match &snapshot {
                    Some(s) => &s.registry,
                    None => {
                        fallback = brief::shortcode::Registry::with_builtins();
                        &fallback
                    }
                };
                registry
                    .map
                    .iter()
                    .filter(|(name, _)| name.starts_with(&prefix))
                    .map(|(name, sc)| {
                        let detail = match sc.kind {
                            ShortKindOpt::Inline => "inline shortcode",
                            ShortKindOpt::Block => "block shortcode",
                            ShortKindOpt::Both => "inline or block shortcode",
                        };
                        item(
                            name,
                            CompletionItemKind::FUNCTION,
                            Some(detail.to_string()),
                            prefix.len(),
                        )
                    })
                    .collect()
            }
            CompletionCtx::RefPath { prefix } => {
                let Some(snapshot) = &snapshot else {
                    return Ok(None);
                };
                let mut paths: BTreeSet<String> =
                    snapshot.index.anchors.keys().cloned().collect();
                for entry in self.docs.iter() {
                    if let Some(p) = &entry.value().path
                        && let Some(rel) = analysis::rel_path(&snapshot.root, p)
                    {
                        paths.insert(rel);
                    }
                }
                paths
                    .iter()
                    .filter(|p| p.starts_with(&prefix))
                    .map(|p| item(p, CompletionItemKind::FILE, None, prefix.len()))
                    .collect()
            }
            CompletionCtx::RefAnchor { path: target, prefix } => {
                let Some(snapshot) = &snapshot else {
                    return Ok(None);
                };
                // Open buffer for the target wins over the disk index.
                let mut anchors: Option<BTreeSet<String>> = None;
                for entry in self.docs.iter() {
                    let matches = entry
                        .value()
                        .path
                        .as_deref()
                        .and_then(|p| analysis::rel_path(&snapshot.root, p))
                        .is_some_and(|rel| rel == target);
                    if matches {
                        anchors = Some(entry.value().anchors.clone());
                        break;
                    }
                }
                let anchors = match anchors {
                    Some(a) => a,
                    None => match snapshot.index.anchors.get(&target) {
                        Some(a) => a.clone(),
                        None => return Ok(None),
                    },
                };
                anchors
                    .iter()
                    .filter(|a| a.starts_with(&prefix))
                    .map(|a| item(a, CompletionItemKind::REFERENCE, None, prefix.len()))
                    .collect()
            }
        };

        Ok(if items.is_empty() {
            None
        } else {
            Some(CompletionResponse::Array(items))
        })
    }

    // ------------------------------------------------------------------
    // Document symbols (heading outline)
    // ------------------------------------------------------------------

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let Some((text, _, _, analysis)) = self.analyze_doc(uri, false) else {
            return Ok(None);
        };
        let Some(doc) = &analysis.doc else {
            return Ok(None);
        };

        let headings = analysis::collect_headings(doc);
        let tree = analysis::nest_headings(&headings);

        fn to_symbols(
            nodes: &[analysis::SymbolNode],
            headings: &[analysis::HeadingInfo],
            text: &str,
        ) -> Vec<DocumentSymbol> {
            nodes
                .iter()
                .map(|n| {
                    let h = &headings[n.idx];
                    let range = span_to_lsp_range(text, h.span.start, h.span.len);
                    let name = if h.text.is_empty() {
                        "(untitled)".to_string()
                    } else {
                        h.text.clone()
                    };
                    let children = to_symbols(&n.children, headings, text);
                    #[allow(deprecated)]
                    DocumentSymbol {
                        name,
                        detail: h.anchor.as_ref().map(|a| format!("#{a}")),
                        kind: SymbolKind::STRING,
                        tags: None,
                        deprecated: None,
                        range,
                        selection_range: range,
                        children: if children.is_empty() {
                            None
                        } else {
                            Some(children)
                        },
                    }
                })
                .collect()
        }

        let symbols = to_symbols(&tree, &headings, &text);
        Ok(if symbols.is_empty() {
            None
        } else {
            Some(DocumentSymbolResponse::Nested(symbols))
        })
    }

    // ------------------------------------------------------------------
    // Formatting
    // ------------------------------------------------------------------

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;

        let text = match self.docs.get(uri) {
            Some(t) => t.text.clone(),
            None => {
                warn!(
                    "brief-lsp: formatting requested for unknown document {:?}",
                    uri
                );
                return Ok(None);
            }
        };

        let opts = fmt::Opts::default();
        let formatted = fmt::format(&text, &opts);

        // If already canonical, return empty list (no edits needed).
        if formatted == text {
            return Ok(Some(vec![]));
        }

        // Single TextEdit covering the entire document.
        let end_pos = byte_offset_to_lsp_position(&text, text.len());
        let full_range = Range {
            start: Position {
                line: 0,
                character: 0,
            },
            end: end_pos,
        };

        Ok(Some(vec![TextEdit {
            range: full_range,
            new_text: formatted,
        }]))
    }
}

// ---------------------------------------------------------------------------
// Helper: LSP position (UTF-16) → byte offset
// ---------------------------------------------------------------------------

/// Convert an LSP `Position` (UTF-16 line/character) back to a byte offset in
/// `text`. Used to locate which construct the cursor is on.
fn lsp_position_to_byte_offset(text: &str, pos: Position) -> usize {
    let mut current_line: u32 = 0;
    let mut line_start_byte: usize = 0;

    // Walk lines to find the right one.
    for (byte_idx, ch) in text.char_indices() {
        if current_line == pos.line {
            // Now walk code units within this line.
            return column_to_byte_offset(&text[line_start_byte..], pos.character)
                + line_start_byte;
        }
        if ch == '\n' {
            current_line += 1;
            line_start_byte = byte_idx + 1;
        }
    }

    // If we're past the end, clamp to text length.
    if current_line == pos.line {
        return column_to_byte_offset(&text[line_start_byte..], pos.character) + line_start_byte;
    }

    text.len()
}

/// Given a line slice and a UTF-16 column, return the byte offset within that
/// line slice.
fn column_to_byte_offset(line: &str, utf16_col: u32) -> usize {
    let mut remaining = utf16_col;
    for (byte_idx, ch) in line.char_indices() {
        if remaining == 0 {
            return byte_idx;
        }
        let units: u32 = if (ch as u32) > 0xFFFF { 2 } else { 1 };
        if remaining < units {
            return byte_idx;
        }
        remaining -= units;
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_conversion_handles_multibyte() {
        // "é" is 2 bytes / 1 UTF-16 unit; "😀" is 4 bytes / 2 UTF-16 units.
        let text = "aé😀b\nsecond";
        // byte → position
        assert_eq!(
            byte_offset_to_lsp_position(text, 0),
            Position { line: 0, character: 0 }
        );
        // after "aé" = 3 bytes → col 2
        assert_eq!(
            byte_offset_to_lsp_position(text, 3),
            Position { line: 0, character: 2 }
        );
        // after "aé😀" = 7 bytes → col 4 (emoji is 2 units)
        assert_eq!(
            byte_offset_to_lsp_position(text, 7),
            Position { line: 0, character: 4 }
        );
        // start of line 1
        let second_start = text.find("second").unwrap();
        assert_eq!(
            byte_offset_to_lsp_position(text, second_start),
            Position { line: 1, character: 0 }
        );

        // position → byte round-trips
        for byte in [0usize, 1, 3, 7, 8, second_start, text.len()] {
            // Only test at char boundaries.
            if text.is_char_boundary(byte) {
                let pos = byte_offset_to_lsp_position(text, byte);
                assert_eq!(lsp_position_to_byte_offset(text, pos), byte, "byte {byte}");
            }
        }
    }

    #[test]
    fn position_past_end_clamps() {
        let text = "one\ntwo";
        assert_eq!(
            lsp_position_to_byte_offset(
                text,
                Position {
                    line: 9,
                    character: 9
                }
            ),
            text.len()
        );
        assert_eq!(
            byte_offset_to_lsp_position(text, 999),
            Position {
                line: 1,
                character: 3
            }
        );
    }
}
