//! The asset kind of a host: the first mapping of the configuration that matches one of the
//! templates linked to the host itself, or one of its tags, else the default kind.

use serde::Deserialize;

use crate::text::{has_tag, same};

/// What of a host a mapping looks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    /// A template linked to the host itself (not one a template links), by its visible name,
    /// compared ignoring case.
    Template(String),
    /// A tag of the host, by name, and with the value when one is given; compared ignoring case.
    Tag { name: String, value: Option<String> },
}

/// A mapping of the configuration: a host that matches gets the kind.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawMapping")]
pub struct KindMapping {
    pub matches: Match,
    /// The asset kind, by its name in the help desk's list.
    pub kind: String,
}

/// A mapping as the configuration file writes it: `template`, or `tag` with an optional `value`,
/// and `kind`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMapping {
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    value: Option<String>,
    kind: String,
}

impl TryFrom<RawMapping> for KindMapping {
    type Error = String;

    fn try_from(raw: RawMapping) -> Result<Self, String> {
        let filled = |s: Option<String>, what: &str| match s {
            Some(s) if s.trim().is_empty() => Err(format!("a mapping's {what} is blank")),
            other => Ok(other.map(|s| s.trim().to_owned())),
        };
        let kind = filled(Some(raw.kind), "kind")?.unwrap_or_default();
        let matches = match (
            filled(raw.template, "template")?,
            filled(raw.tag, "tag")?,
            filled(raw.value, "value")?,
        ) {
            (Some(template), None, None) => Match::Template(template),
            (None, Some(name), value) => Match::Tag { name, value },
            (Some(_), Some(_), _) => {
                return Err(format!(
                    "the mapping to {kind} names both a template and a tag: give one"
                ));
            }
            (Some(_), None, Some(_)) => {
                return Err(format!("the mapping to {kind} gives a value without a tag"));
            }
            (None, None, _) => {
                return Err(format!(
                    "the mapping to {kind} names neither a template nor a tag"
                ));
            }
        };
        Ok(KindMapping { matches, kind })
    }
}

/// What of a host decides its kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostTraits {
    /// The visible names of the templates linked to the host itself.
    pub templates: Vec<String>,
    /// The host's tags, as name and value.
    pub tags: Vec<(String, String)>,
}

/// The mappings in their order, with the kind of a host none of them matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kinds {
    pub mappings: Vec<KindMapping>,
    pub default: String,
}

impl KindMapping {
    fn matches(&self, host: &HostTraits) -> bool {
        match &self.matches {
            Match::Template(template) => host.templates.iter().any(|t| same(t, template)),
            Match::Tag { name, value } => has_tag(&host.tags, name, value.as_deref()),
        }
    }
}

impl Kinds {
    /// The kind of the first mapping that matches the host, else the default.
    pub fn kind_of(&self, host: &HostTraits) -> &str {
        self.mappings
            .iter()
            .find(|m| m.matches(host))
            .map_or(&self.default, |m| &m.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(name: &str, kind: &str) -> KindMapping {
        KindMapping {
            matches: Match::Template(name.into()),
            kind: kind.into(),
        }
    }

    fn tag(name: &str, value: Option<&str>, kind: &str) -> KindMapping {
        KindMapping {
            matches: Match::Tag {
                name: name.into(),
                value: value.map(Into::into),
            },
            kind: kind.into(),
        }
    }

    fn host(templates: &[&str], tags: &[(&str, &str)]) -> HostTraits {
        HostTraits {
            templates: templates.iter().map(|t| (*t).to_owned()).collect(),
            tags: tags
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn the_first_mapping_that_matches_decides_else_the_default() {
        let kinds = Kinds {
            mappings: vec![
                tag("device", Some("printer"), "Printer"),
                template("Linux by Zabbix agent", "Server"),
                tag("role", None, "Appliance"),
            ],
            default: "Network device".into(),
        };
        let cases = [
            (host(&["Linux by Zabbix agent"], &[]), "Server"),
            (host(&["linux BY zabbix agent"], &[]), "Server"),
            // Both match: the tag's mapping comes first.
            (
                host(&["Linux by Zabbix agent"], &[("Device", "PRINTER")]),
                "Printer",
            ),
            // A tag with another value does not match; one without a value matches any.
            (host(&[], &[("device", "scanner")]), "Network device"),
            (host(&[], &[("role", "")]), "Appliance"),
            (host(&["Windows by Zabbix agent"], &[]), "Network device"),
            (host(&[], &[]), "Network device"),
        ];
        for (traits, kind) in cases {
            assert_eq!(kinds.kind_of(&traits), kind, "{traits:?}");
        }

        // The other way round, the template's mapping comes first and decides.
        let kinds = Kinds {
            mappings: vec![
                template("Linux by Zabbix agent", "Server"),
                tag("device", Some("printer"), "Printer"),
            ],
            default: "Network device".into(),
        };
        let both = host(&["Linux by Zabbix agent"], &[("device", "printer")]);
        assert_eq!(kinds.kind_of(&both), "Server");
        assert_eq!(
            kinds.kind_of(&host(&[], &[("device", "printer")])),
            "Printer"
        );
    }

    #[test]
    fn a_mapping_names_a_template_or_a_tag_and_a_kind() {
        let read = |toml: &str| -> Result<KindMapping, String> {
            toml::from_str(toml).map_err(|e| e.message().to_owned())
        };
        assert_eq!(
            read("template = \" Linux by Zabbix agent \"\nkind = \"Server\""),
            Ok(template("Linux by Zabbix agent", "Server"))
        );
        assert_eq!(
            read("tag = \"role\"\nvalue = \"web\"\nkind = \"Server\""),
            Ok(tag("role", Some("web"), "Server"))
        );
        assert_eq!(
            read("tag = \"role\"\nkind = \"Server\""),
            Ok(tag("role", None, "Server"))
        );
        for (toml, problem) in [
            (
                "template = \"Linux\"\ntag = \"role\"\nkind = \"Server\"",
                "the mapping to Server names both a template and a tag: give one",
            ),
            (
                "kind = \"Server\"",
                "the mapping to Server names neither a template nor a tag",
            ),
            (
                "template = \"Linux\"\nvalue = \"web\"\nkind = \"Server\"",
                "the mapping to Server gives a value without a tag",
            ),
            (
                "template = \"Linux\"\nkind = \" \"",
                "a mapping's kind is blank",
            ),
            ("tag = \" \"\nkind = \"Server\"", "a mapping's tag is blank"),
        ] {
            assert_eq!(read(toml), Err(problem.to_owned()), "{toml}");
        }
        assert!(
            read("template = \"Linux\"\nkind = \"Server\"\nclass = \"x\"")
                .unwrap_err()
                .contains("class")
        );
    }
}
