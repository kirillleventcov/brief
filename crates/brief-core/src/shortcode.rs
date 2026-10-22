//! Shortcode declarations: the argument value model, the per-shortcode
//! schema loaded from `brief.toml`, and the [`Registry`] of built-ins plus
//! user-declared shortcodes that resolve checks invocations against.

use serde::Deserialize;
use std::collections::BTreeMap;

/// A parsed shortcode argument value.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ArgValue {
    Ident(String),
    Int(i64),
    Str(String),
    Array(Vec<ArgValue>),
}

impl ArgValue {
    /// The user-facing name of this value's type, for diagnostics.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            ArgValue::Ident(_) => "ident",
            ArgValue::Int(_) => "int",
            ArgValue::Str(_) => "string",
            ArgValue::Array(_) => "array",
        }
    }

    /// The text of a string or identifier value; `None` for other types.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ArgValue::Str(s) | ArgValue::Ident(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

/// The declared type of a shortcode argument (`type = "..."` in
/// `brief.toml`).
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ArgType {
    String,
    Int,
    Ident,
    Array,
}

/// Schema of one shortcode argument.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArgSpec {
    #[serde(rename = "type")]
    pub ty: ArgType,
    #[serde(default)]
    pub required: bool,
    /// 1-based position when the argument may be passed positionally.
    #[serde(default)]
    pub position: Option<usize>,
    /// The only values the argument accepts, if restricted.
    #[serde(default)]
    pub oneof: Option<Vec<String>>,
}

/// Where a shortcode may be used: inline (`@name[...]`), as a block
/// (`@name ... @end`), or both.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShortcodeKind {
    #[default]
    Inline,
    Block,
    Both,
}

/// Declaration of one shortcode: its form, arguments and optional output
/// templates.
#[derive(Clone, Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Shortcode {
    #[serde(default)]
    pub kind: ShortcodeKind,
    #[serde(default)]
    pub arguments: BTreeMap<String, ArgSpec>,
    /// Template for HTML output; `None` uses the built-in rendering.
    #[serde(default)]
    pub template_html: Option<String>,
    /// Template for LLM output; `None` uses the built-in rendering.
    #[serde(default)]
    pub template_llm: Option<String>,
}

/// Every known shortcode by name. Extending it with user declarations
/// replaces built-ins of the same name.
#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub map: BTreeMap<String, Shortcode>,
}

impl Registry {
    /// A registry holding only the built-in shortcodes.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut m = BTreeMap::new();

        m.insert(
            "link".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
                arguments: {
                    let mut a = BTreeMap::new();
                    a.insert(
                        "url".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: true,
                            position: Some(1),
                            oneof: None,
                        },
                    );
                    a.insert(
                        "title".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: false,
                            position: None,
                            oneof: None,
                        },
                    );
                    a
                },
                template_html: None,
                template_llm: None,
            },
        );

        m.insert(
            "image".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
                arguments: {
                    let mut a = BTreeMap::new();
                    a.insert(
                        "src".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: true,
                            position: None,
                            oneof: None,
                        },
                    );
                    a.insert(
                        "alt".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: false,
                            position: None,
                            oneof: None,
                        },
                    );
                    a
                },
                ..Default::default()
            },
        );

        m.insert(
            "kbd".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
                ..Default::default()
            },
        );

        // Inline subscript and superscript: replacements for the only inline
        // HTML constructs Brief still wants to express. Both take their text
        // via the `[content]` body — no arguments.
        m.insert(
            "sub".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
                ..Default::default()
            },
        );

        m.insert(
            "sup".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
                ..Default::default()
            },
        );

        // Collapsible block: `@details(summary: "...") ... @end` ↔
        // `<details><summary>...</summary>...</details>`.
        m.insert(
            "details".into(),
            Shortcode {
                kind: ShortcodeKind::Block,
                arguments: {
                    let mut a = BTreeMap::new();
                    a.insert(
                        "summary".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: true,
                            position: None,
                            oneof: None,
                        },
                    );
                    a
                },
                ..Default::default()
            },
        );

        m.insert(
            "dl".into(),
            Shortcode {
                kind: ShortcodeKind::Block,
                ..Default::default()
            },
        );

        m.insert(
            "t".into(),
            Shortcode {
                kind: ShortcodeKind::Block,
                arguments: {
                    let mut a = BTreeMap::new();
                    a.insert(
                        "align".into(),
                        ArgSpec {
                            ty: ArgType::Array,
                            required: false,
                            position: None,
                            oneof: None,
                        },
                    );
                    a
                },
                ..Default::default()
            },
        );

        m.insert(
            "code".into(),
            Shortcode {
                kind: ShortcodeKind::Block,
                arguments: {
                    let mut a = BTreeMap::new();
                    a.insert(
                        "lang".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: false,
                            position: Some(1),
                            oneof: None,
                        },
                    );
                    a
                },
                ..Default::default()
            },
        );

        m.insert(
            "callout".into(),
            Shortcode {
                kind: ShortcodeKind::Block,
                arguments: {
                    let mut a = BTreeMap::new();
                    a.insert(
                        "kind".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: true,
                            position: None,
                            oneof: Some(vec![
                                "note".into(),
                                "tip".into(),
                                "important".into(),
                                "warning".into(),
                                "caution".into(),
                            ]),
                        },
                    );
                    a
                },
                ..Default::default()
            },
        );

        m.insert(
            "math".into(),
            Shortcode {
                kind: ShortcodeKind::Both,
                ..Default::default()
            },
        );

        m.insert(
            "footnote".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
                ..Default::default()
            },
        );

        m.insert(
            "ref".into(),
            Shortcode {
                kind: ShortcodeKind::Inline,
                arguments: {
                    let mut a = BTreeMap::new();
                    a.insert(
                        "title".into(),
                        ArgSpec {
                            ty: ArgType::String,
                            required: true,
                            position: Some(1),
                            oneof: None,
                        },
                    );
                    a
                },
                template_html: None,
                template_llm: None,
            },
        );

        Registry { map: m }
    }

    /// The declaration of shortcode `name`, if registered.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Shortcode> {
        self.map.get(name)
    }
}

impl Extend<(String, Shortcode)> for Registry {
    fn extend<I: IntoIterator<Item = (String, Shortcode)>>(&mut self, iter: I) {
        self.map.extend(iter);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_is_a_builtin_inline_shortcode() {
        let reg = Registry::with_builtins();
        let sc = reg.get("ref").expect("@ref must be a built-in");
        assert!(matches!(sc.kind, ShortcodeKind::Inline));
        let title = sc.arguments.get("title").expect("title arg");
        assert!(title.required);
        assert_eq!(title.position, Some(1));
        assert!(matches!(title.ty, ArgType::String));
    }

    #[test]
    fn dl_is_a_builtin_block_shortcode() {
        let reg = Registry::with_builtins();
        let sc = reg.get("dl").expect("@dl must be a built-in");
        assert!(matches!(sc.kind, ShortcodeKind::Block));
        assert!(sc.arguments.is_empty(), "@dl takes no arguments in v0.3");
    }
}
