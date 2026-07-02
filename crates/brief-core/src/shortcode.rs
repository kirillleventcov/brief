use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum ArgValue {
    Ident(String),
    Int(i64),
    Str(String),
    Array(Vec<ArgValue>),
}

impl ArgValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            ArgValue::Ident(_) => "ident",
            ArgValue::Int(_) => "int",
            ArgValue::Str(_) => "string",
            ArgValue::Array(_) => "array",
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ArgValue::Str(s) | ArgValue::Ident(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArgType {
    String,
    Int,
    Ident,
    Array,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArgSpec {
    #[serde(rename = "type")]
    pub ty: ArgType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub position: Option<usize>,
    #[serde(default)]
    pub oneof: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ShortKindOpt {
    Inline,
    Block,
    Both,
}

impl Default for ShortKindOpt {
    fn default() -> Self {
        ShortKindOpt::Inline
    }
}

#[derive(Clone, Debug, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Shortcode {
    #[serde(default)]
    pub kind: ShortKindOpt,
    #[serde(default)]
    pub arguments: BTreeMap<String, ArgSpec>,
    #[serde(default)]
    pub template_html: Option<String>,
    #[serde(default)]
    pub template_llm: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub map: BTreeMap<String, Shortcode>,
}

impl Registry {
    pub fn with_builtins() -> Self {
        let mut m = BTreeMap::new();

        m.insert(
            "link".into(),
            Shortcode {
                kind: ShortKindOpt::Inline,
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
                kind: ShortKindOpt::Inline,
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
                kind: ShortKindOpt::Inline,
                ..Default::default()
            },
        );

        // Inline subscript and superscript: replacements for the only inline
        // HTML constructs Brief still wants to express. Both take their text
        // via the `[content]` body — no arguments.
        m.insert(
            "sub".into(),
            Shortcode {
                kind: ShortKindOpt::Inline,
                ..Default::default()
            },
        );

        m.insert(
            "sup".into(),
            Shortcode {
                kind: ShortKindOpt::Inline,
                ..Default::default()
            },
        );

        // Collapsible block: `@details(summary: "...") ... @end` ↔
        // `<details><summary>...</summary>...</details>`.
        m.insert(
            "details".into(),
            Shortcode {
                kind: ShortKindOpt::Block,
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
                kind: ShortKindOpt::Block,
                ..Default::default()
            },
        );

        m.insert(
            "t".into(),
            Shortcode {
                kind: ShortKindOpt::Block,
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
                kind: ShortKindOpt::Block,
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
                kind: ShortKindOpt::Block,
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
                kind: ShortKindOpt::Both,
                ..Default::default()
            },
        );

        m.insert(
            "footnote".into(),
            Shortcode {
                kind: ShortKindOpt::Inline,
                ..Default::default()
            },
        );

        m.insert(
            "ref".into(),
            Shortcode {
                kind: ShortKindOpt::Inline,
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

    pub fn get(&self, name: &str) -> Option<&Shortcode> {
        self.map.get(name)
    }

    pub fn extend(&mut self, other: BTreeMap<String, Shortcode>) {
        for (k, v) in other {
            self.map.insert(k, v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_is_a_builtin_inline_shortcode() {
        let reg = Registry::with_builtins();
        let sc = reg.get("ref").expect("@ref must be a built-in");
        assert!(matches!(sc.kind, ShortKindOpt::Inline));
        let title = sc.arguments.get("title").expect("title arg");
        assert!(title.required);
        assert_eq!(title.position, Some(1));
        assert!(matches!(title.ty, ArgType::String));
    }

    #[test]
    fn dl_is_a_builtin_block_shortcode() {
        let reg = Registry::with_builtins();
        let sc = reg.get("dl").expect("@dl must be a built-in");
        assert!(matches!(sc.kind, ShortKindOpt::Block));
        assert!(sc.arguments.is_empty(), "@dl takes no arguments in v0.3");
    }
}
