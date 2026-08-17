//! Source-format conversion: Markdown ↔ Brief.
//!
//! `to_brief` migrates Markdown into Brief (`brief convert doc.md`);
//! `to_md` exports Brief back out to Markdown (`brief convert doc.brf`).
//! Both directions are lossy-with-diagnostics: every construct that has no
//! clean equivalent on the other side is reported, nothing is silently
//! dropped.

mod to_brief;
mod to_md;

pub use to_brief::{ConvertResult, Diag, Hole, convert};
pub use to_md::{MdDiag, MdHole, MdResult, to_markdown};
