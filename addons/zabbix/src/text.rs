//! How the add-on compares the names of its configuration with Zabbix's, and lists them in its
//! sentences.

/// The same text, ignoring case and surrounding whitespace.
pub fn same(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Whether `tags`, as name and value, have one named `name`, with `value` when one is given;
/// compared ignoring case.
pub fn has_tag(tags: &[(String, String)], name: &str, value: Option<&str>) -> bool {
    tags.iter()
        .any(|(tag, tag_value)| same(tag, name) && value.is_none_or(|v| same(tag_value, v)))
}

/// `items` as a sentence lists them, joined by `last`: `a`, `a or b`, `a, b or c`.
pub fn list<T: AsRef<str>>(items: &[T], last: &str) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_owned(),
        [init @ .., end] => {
            let init: Vec<&str> = init.iter().map(AsRef::as_ref).collect();
            format!("{} {last} {}", init.join(", "), end.as_ref())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_joins_its_last_item_with_the_word_given() {
        assert_eq!(list::<&str>(&[], "or"), "");
        assert_eq!(list(&["low"], "or"), "low");
        assert_eq!(list(&["low", "high"], "or"), "low or high");
        assert_eq!(list(&["a", "b", "c"], "and"), "a, b and c");
    }

    #[test]
    fn a_tag_is_found_by_name_and_by_value_when_one_is_given_ignoring_case() {
        let tags = vec![
            ("Service".to_owned(), "Mail".to_owned()),
            ("scope".to_owned(), String::new()),
        ];
        assert!(has_tag(&tags, "service", Some("mail")));
        assert!(has_tag(&tags, "service", None));
        assert!(has_tag(&tags, " SCOPE ", None));
        assert!(!has_tag(&tags, "service", Some("web")));
        assert!(!has_tag(&tags, "site", None));
    }
}
