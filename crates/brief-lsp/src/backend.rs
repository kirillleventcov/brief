//! LSP backend for the Brief markup language.
//!
//! Position encoding strategy: UTF-16 (the default required by the LSP
//! specification when no alternative is negotiated). All byte offsets coming
//! out of brief-core are converted to UTF-16 code-unit columns via the helper
//! `byte_offset_to_lsp_position`. This avoids any per-client negotiation
//! complexity and is compatible with every editor out of the box.

use dashmap::DashMap;
use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, ls_types::*};
use tracing::{info, warn};

// The crate is named `brief-core` in Cargo.toml but the Rust crate name
// (lib.name) is `brief` — match that.
use brief::{diag::Severity, fmt, lexer, parser, span::SourceMap, validate};

// ---------------------------------------------------------------------------
// Explain text table (mirrors run_explain in brief-cli, kept in sync manually)
// ---------------------------------------------------------------------------

/// Returns the long-form explanation string for a given error code string like
/// "B0703", or `None` if the code is unknown.
fn explain_code(code_str: &str) -> Option<&'static str> {
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
            "Shortcodes must be registered in `brief.toml` under `[shortcodes.<name>]` (or be a built-in: link, image, kbd, sub, sup, details, t, code, callout, math, footnote, ref). Note: `@br` is intentionally not a shortcode — use `\\` at end of line for a hard break.",
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
        (
            Code::RefMissingFile,
            "Brief verifies cross-document references at compile time. The file referenced by `@ref[path.brf]` was not found anywhere under the project root (the directory containing `brief.toml`). Either fix the path, create the missing file, or move the file into the project tree.",
        ),
        (
            Code::RefMissingAnchor,
            "Brief verifies that the `#anchor` portion of `@ref[file.brf#anchor]` matches a heading anchor declared in the target file (e.g. `## Title {#anchor}`). The anchor was not found. The diagnostic's help text lists the anchors that *do* exist in the target file.",
        ),
        (
            Code::RefBadTarget,
            "`@ref` targets are project-relative `.brf` paths, optionally suffixed with `#anchor`. Leading `/`, `..` segments, backslashes, missing `.brf` extension, and anchors not matching `[a-z0-9-]+` are rejected. Restate the target in canonical form.",
        ),
        (
            Code::RefNoProject,
            "`@ref` only works inside a project rooted by a `brief.toml` file. The compiler walks up from the source file looking for one. Create a `brief.toml` (an empty file is fine) at the desired root, or remove the `@ref` invocation.",
        ),
    ];
    for (c, text) in table {
        if c.as_str() == code_str {
            return Some(text);
        }
    }
    None
}

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

/// In-memory document store: Uri → source text.
type DocStore = DashMap<Uri, String>;

#[derive(Debug)]
pub struct Backend {
    client: Client,
    docs: DocStore,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            docs: DashMap::new(),
        }
    }

    /// Run brief-core's lex+parse+validate pipeline on `text` and push
    /// `textDocument/publishDiagnostics` to the client.
    async fn publish_diagnostics_for(&self, uri: Uri, text: &str) {
        let src = SourceMap::new(uri.as_str(), text.to_string());

        // Collect all diagnostics from lex → parse → validate.
        let all_diags: Vec<brief::diag::Diagnostic> = match lexer::lex(&src) {
            Err(lex_diags) => lex_diags,
            Ok(tokens) => {
                let (doc, mut parse_diags) = parser::parse(tokens, &src);
                let opts = validate::ValidateOpts::default();
                parse_diags.extend(validate::validate(&doc, &opts, &src));
                parse_diags
            }
        };

        // Convert to LSP diagnostics.
        let lsp_diags: Vec<Diagnostic> = all_diags
            .iter()
            .map(|d| {
                let range = span_to_lsp_range(text, d.span.start, d.span.len);
                let code_str = d.code.as_str();
                let message = format!("{}: {}", code_str, d.code.message());
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

        self.client.publish_diagnostics(uri, lsp_diags, None).await;
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
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
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
        let text = params.text_document.text;
        info!("brief-lsp: did_open {:?}", uri);
        self.docs.insert(uri.clone(), text.clone());
        self.publish_diagnostics_for(uri, &text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        // TextDocumentSyncKind::FULL — the single change event contains the
        // complete new text.
        if let Some(change) = params.content_changes.into_iter().last() {
            let text = change.text;
            info!("brief-lsp: did_change {:?}", uri);
            self.docs.insert(uri.clone(), text.clone());
            self.publish_diagnostics_for(uri, &text).await;
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

        let text = match self.docs.get(uri) {
            Some(t) => t.clone(),
            None => {
                warn!("brief-lsp: hover requested for unknown document {:?}", uri);
                return Ok(None);
            }
        };

        let src = SourceMap::new(uri.as_str(), text.clone());

        // Gather diagnostics.
        let all_diags: Vec<brief::diag::Diagnostic> = match lexer::lex(&src) {
            Err(lex_diags) => lex_diags,
            Ok(tokens) => {
                let (doc, mut parse_diags) = parser::parse(tokens, &src);
                let opts = validate::ValidateOpts::default();
                parse_diags.extend(validate::validate(&doc, &opts, &src));
                parse_diags
            }
        };

        // Convert cursor position (UTF-16) to a byte offset so we can check
        // whether it falls inside a diagnostic span.
        let cursor_byte = lsp_position_to_byte_offset(&text, pos);

        // Find the first diagnostic whose span contains the cursor.
        let hit = all_diags.iter().find(|d| {
            let s = d.span.start as usize;
            let e = s + d.span.len as usize;
            cursor_byte >= s && cursor_byte <= e
        });

        let hit = match hit {
            Some(h) => h,
            None => return Ok(None),
        };

        let code_str = hit.code.as_str();
        let mut contents = format!(
            "**{}**: {}\n\nRun `brief explain {}` for details.",
            code_str,
            hit.code.message(),
            code_str,
        );

        // If brief-core exposes the long-form explain text, inline it.
        if let Some(detail) = explain_code(&code_str) {
            contents.push_str("\n\n---\n\n");
            contents.push_str(detail);
        }

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
    // Formatting
    // ------------------------------------------------------------------

    async fn formatting(&self, params: DocumentFormattingParams) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;

        let text = match self.docs.get(uri) {
            Some(t) => t.clone(),
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
/// `text`. Used in the hover handler to locate which diagnostic the cursor is
/// on.
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
