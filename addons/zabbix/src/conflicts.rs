//! The conflicts the add-on reports: hosts in host groups that different customers cover, which
//! it neither imports nor updates, each with a sentence in English and Italian that those who
//! manage add-ons can act on, within the limits of the add-on contract.

use std::collections::HashMap;

use hd_addon_sdk::Conflict;
use uuid::Uuid;

use crate::coverage::{Cover, CoveredGroup};

/// The longest sentence the core takes, in characters.
pub const MAX_SENTENCE: usize = 500;
/// The most conflicts the core takes in one list.
pub const MAX_CONFLICTS: usize = 1000;
/// At most this many characters of a host's name in a sentence: room for its groups.
const HOST_SHOWN: usize = 100;
/// At most this many characters of a group's name in a sentence.
const GROUP_SHOWN: usize = 80;
/// At most this many characters of a customer's name in a sentence.
const CUSTOMER_SHOWN: usize = 60;

/// A host the add-on does not import because its groups are covered by different customers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConflict {
    /// The host's visible name.
    pub host: String,
    /// The host's groups that are covered.
    pub groups: Vec<CoveredGroup>,
}

/// `text` cut to at most `max` characters, ending in `…` when cut.
fn shortened(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// The sentences of one language.
struct Language {
    tag: &'static str,
    offboarded: &'static str,
    /// The groups left out of a sentence, counted.
    more: fn(usize) -> String,
    /// A host, and its groups with their customers.
    sentence: fn(&str, &str) -> String,
    /// The hosts left out of the list, counted, and how many it shows.
    overflow: fn(usize, usize) -> String,
}

const LANGUAGES: [Language; 2] = [
    Language {
        tag: "en",
        offboarded: "an offboarded customer",
        more: |n| format!(" and {n} more"),
        sentence: |host, groups| {
            format!(
                "The host {host} is in host groups of different customers: {groups}. It is \
                 neither imported nor updated: leave it in the groups of one customer."
            )
        },
        overflow: |more, listed| {
            format!(
                "{more} more hosts are in host groups of different customers and are neither \
                 imported nor updated; the list shows the first {listed}."
            )
        },
    },
    Language {
        tag: "it",
        offboarded: "un cliente dismesso",
        more: |n| format!(" e altri {n}"),
        sentence: |host, groups| {
            format!(
                "L'host {host} è in gruppi di host di clienti diversi: {groups}. Non viene né \
                 importato né aggiornato: lasciarlo nei gruppi di un solo cliente."
            )
        },
        overflow: |more, listed| {
            format!(
                "Altri {more} host sono in gruppi di host di clienti diversi e non vengono né \
                 importati né aggiornati; l'elenco mostra i primi {listed}."
            )
        },
    },
];

impl HostConflict {
    /// The conflict as the core takes it, naming the customers by `names` (by id).
    pub fn to_conflict(&self, names: &HashMap<Uuid, String>) -> Conflict {
        let host = shortened(&self.host, HOST_SHOWN);
        Conflict::new(LANGUAGES.iter().map(|language| {
            let items: Vec<String> = self
                .groups
                .iter()
                .map(|group| {
                    let who = match &group.cover {
                        Cover::Customer(id) => names
                            .get(id)
                            .map_or_else(|| id.to_string(), |n| shortened(n, CUSTOMER_SHOWN)),
                        Cover::Offboarded => language.offboarded.to_owned(),
                    };
                    format!("{} ({who})", shortened(&group.name, GROUP_SHOWN))
                })
                .collect();
            // As many groups as fit, the others counted.
            let mut shown = items.len();
            let sentence = loop {
                let mut groups = items[..shown].join(", ");
                if shown < items.len() {
                    groups.push_str(&(language.more)(items.len() - shown));
                }
                let sentence = (language.sentence)(&host, &groups);
                if sentence.chars().count() <= MAX_SENTENCE || shown == 1 {
                    break sentence;
                }
                shown -= 1;
            };
            (language.tag, shortened(&sentence, MAX_SENTENCE))
        }))
    }
}

/// The list of conflicts as the core takes it: at most [`MAX_CONFLICTS`], the last one counting
/// the hosts left out of the list when there are more.
pub fn report(conflicts: &[HostConflict], names: &HashMap<Uuid, String>) -> Vec<Conflict> {
    if conflicts.len() <= MAX_CONFLICTS {
        return conflicts.iter().map(|c| c.to_conflict(names)).collect();
    }
    let listed = MAX_CONFLICTS - 1;
    let more = conflicts.len() - listed;
    let mut report: Vec<Conflict> = conflicts[..listed]
        .iter()
        .map(|c| c.to_conflict(names))
        .collect();
    report.push(Conflict::new(
        LANGUAGES
            .iter()
            .map(|language| (language.tag, (language.overflow)(more, listed))),
    ));
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIGHTFIELD: Uuid = Uuid::from_u128(1);
    const HARBOR: Uuid = Uuid::from_u128(2);

    fn names() -> HashMap<Uuid, String> {
        HashMap::from([
            (BRIGHTFIELD, "Brightfield".to_owned()),
            (HARBOR, "Harbor Lane".to_owned()),
        ])
    }

    #[test]
    fn a_conflict_names_the_host_and_its_groups_with_their_customers_in_english_and_italian() {
        let conflict = HostConflict {
            host: "db-03".into(),
            groups: vec![
                CoveredGroup::new("Brightfield/Servers", Cover::Customer(BRIGHTFIELD)),
                CoveredGroup::new("Harbor Lane", Cover::Customer(HARBOR)),
                CoveredGroup::new("Old Quay", Cover::Offboarded),
            ],
        };
        let sentence = conflict.to_conflict(&names()).sentence;
        assert_eq!(
            sentence["en"],
            "The host db-03 is in host groups of different customers: Brightfield/Servers \
             (Brightfield), Harbor Lane (Harbor Lane), Old Quay (an offboarded customer). It is \
             neither imported nor updated: leave it in the groups of one customer."
        );
        assert_eq!(
            sentence["it"],
            "L'host db-03 è in gruppi di host di clienti diversi: Brightfield/Servers \
             (Brightfield), Harbor Lane (Harbor Lane), Old Quay (un cliente dismesso). Non viene \
             né importato né aggiornato: lasciarlo nei gruppi di un solo cliente."
        );
    }

    #[test]
    fn a_sentence_keeps_within_the_limit_counting_the_groups_left_out() {
        let conflict = HostConflict {
            host: "h".repeat(128),
            groups: (0..20)
                .map(|i| {
                    let cover = if i % 2 == 0 {
                        Cover::Customer(BRIGHTFIELD)
                    } else {
                        Cover::Customer(HARBOR)
                    };
                    CoveredGroup::new(format!("{i:02}/{}", "g".repeat(250)), cover)
                })
                .collect(),
        };
        let sentence = conflict.to_conflict(&names()).sentence;
        for language in ["en", "it"] {
            let text = &sentence[language];
            assert!(text.chars().count() <= MAX_SENTENCE, "{text}");
            assert!(text.contains(&format!("{}…", "h".repeat(99))), "{text}");
        }
        assert!(
            sentence["en"].contains(" and 18 more. "),
            "{}",
            sentence["en"]
        );
        assert!(
            sentence["it"].contains(" e altri 18. "),
            "{}",
            sentence["it"]
        );
    }

    #[test]
    fn a_list_keeps_within_the_limit_counting_the_hosts_left_out() {
        let conflicts: Vec<HostConflict> = (0..1200)
            .map(|i| HostConflict {
                host: format!("host-{i}"),
                groups: vec![
                    CoveredGroup::new("Brightfield", Cover::Customer(BRIGHTFIELD)),
                    CoveredGroup::new("Harbor Lane", Cover::Customer(HARBOR)),
                ],
            })
            .collect();
        let report = report(&conflicts, &names());
        assert_eq!(report.len(), MAX_CONFLICTS);
        assert!(report[998].sentence["en"].starts_with("The host host-998 "));
        assert_eq!(
            report[999].sentence["en"],
            "201 more hosts are in host groups of different customers and are neither imported \
             nor updated; the list shows the first 999."
        );
        assert!(report[999].sentence["it"].starts_with("Altri 201 host "));
    }
}
