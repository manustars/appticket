//! The asset kind of a host: the first mapping of the configuration that matches one of the
//! host's templates or tags, else the default kind.

use serde::Deserialize;

/// A mapping of the configuration: a host linked to the template, or with the tag (and the
/// value, when one is given), gets the kind.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindMapping {
    /// A template the host is linked to, by its visible name, compared ignoring case.
    #[serde(default)]
    pub template: Option<String>,
    /// A tag of the host, by name, compared ignoring case.
    #[serde(default)]
    pub tag: Option<String>,
    /// With `tag`, the tag's value, compared ignoring case; without it any value matches.
    #[serde(default)]
    pub value: Option<String>,
    /// The asset kind, by its name in the help desk's list.
    pub kind: String,
}

/// What of a host decides its kind.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostTraits {
    /// The visible names of the templates the host is linked to.
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
    /// Why the mapping cannot be used, if it cannot.
    pub fn problem(&self) -> Option<&'static str> {
        let blank = |s: &Option<String>| s.as_deref().is_some_and(|s| s.trim().is_empty());
        if self.kind.trim().is_empty() {
            return Some("its kind is blank");
        }
        if blank(&self.template) || blank(&self.tag) || blank(&self.value) {
            return Some("it has a blank template, tag or value");
        }
        match (&self.template, &self.tag, &self.value) {
            (Some(_), Some(_), _) => Some("it names both a template and a tag: give one"),
            (None, None, _) => Some("it names neither a template nor a tag"),
            (Some(_), None, Some(_)) => Some("it gives a value without a tag"),
            _ => None,
        }
    }

    fn matches(&self, host: &HostTraits) -> bool {
        let same = |a: &str, b: &str| a.trim().to_lowercase() == b.trim().to_lowercase();
        match (&self.template, &self.tag) {
            (Some(template), _) => host.templates.iter().any(|t| same(t, template)),
            (None, Some(tag)) => host.tags.iter().any(|(name, value)| {
                same(name, tag) && self.value.as_deref().is_none_or(|v| same(value, v))
            }),
            (None, None) => false,
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
            .trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(name: &str, kind: &str) -> KindMapping {
        KindMapping {
            template: Some(name.into()),
            tag: None,
            value: None,
            kind: kind.into(),
        }
    }

    fn tag(name: &str, value: Option<&str>, kind: &str) -> KindMapping {
        KindMapping {
            template: None,
            tag: Some(name.into()),
            value: value.map(Into::into),
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
            // Both match: the first one decides.
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
    }

    #[test]
    fn a_mapping_names_a_template_or_a_tag_and_a_kind() {
        let mut both = template("Linux by Zabbix agent", "Server");
        both.tag = Some("role".into());
        let mut value_alone = template("Linux by Zabbix agent", "Server");
        value_alone.value = Some("web".into());
        let neither = KindMapping {
            template: None,
            tag: None,
            value: None,
            kind: "Server".into(),
        };
        for (mapping, problem) in [
            (template("Linux by Zabbix agent", "Server"), None),
            (tag("role", Some("web"), "Server"), None),
            (both, Some("it names both a template and a tag: give one")),
            (neither, Some("it names neither a template nor a tag")),
            (value_alone, Some("it gives a value without a tag")),
            (template("Linux", " "), Some("its kind is blank")),
            (
                tag(" ", None, "Server"),
                Some("it has a blank template, tag or value"),
            ),
        ] {
            assert_eq!(mapping.problem(), problem, "{mapping:?}");
        }
    }
}
