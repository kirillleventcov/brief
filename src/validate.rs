use crate::ast::{Block, Document};
use crate::diag::{Code, Diagnostic};

pub struct ValidateOpts {
    pub strict_heading_levels: bool,
}

impl Default for ValidateOpts {
    fn default() -> Self {
        ValidateOpts {
            strict_heading_levels: false,
        }
    }
}

pub fn validate(doc: &Document, opts: &ValidateOpts) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    if opts.strict_heading_levels {
        check_heading_monotonic(doc, &mut diags);
    }
    diags
}

fn check_heading_monotonic(doc: &Document, diags: &mut Vec<Diagnostic>) {
    let mut last: u8 = 0;
    for b in &doc.blocks {
        if let Block::Heading { level, span, .. } = b {
            if last > 0 && *level > last + 1 {
                diags.push(
                    Diagnostic::new(Code::HeadingMonotonic, *span)
                        .label(format!("heading level jumps from {} to {}", last, level))
                        .help("increment heading levels by at most one"),
                );
            }
            last = *level;
        }
    }
}
