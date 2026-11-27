//! Which customer each host group and each host is of, from the external references customers
//! have in the add-on's system (docs/domain-rules.md, "Add-ons"): a group a customer has covers
//! the groups nested in it, unless one of them is another customer's, which then covers it and
//! the groups nested in it; a group an offboarded customer has is covered, with the groups nested
//! in it, and nothing of them is imported. Zabbix nests host groups by name (`Parent/Child`), and
//! a group may exist without its parent: it is nested in the nearest one that exists.

use std::collections::{BTreeSet, HashMap};

use hd_addon_sdk::api::{Customer, ExternalReference};
use hd_addon_sdk::names::{SystemName, SystemNames};
use uuid::Uuid;

/// A host group of Zabbix, by id and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostGroup {
    pub id: String,
    pub name: String,
}

/// Who covers a host group.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Cover {
    /// An active customer, by id.
    Customer(Uuid),
    /// An offboarded customer, which add-ons do not read: nothing of it is imported.
    Offboarded,
}

/// A host group that an external reference covers, by name, with who covers it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoveredGroup {
    pub name: String,
    pub cover: Cover,
}

impl CoveredGroup {
    pub fn new(name: impl Into<String>, cover: Cover) -> Self {
        CoveredGroup {
            name: name.into(),
            cover,
        }
    }
}

/// Where a host goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// It becomes an asset of this customer.
    Customer(Uuid),
    /// No external reference covers any of its groups: it is not imported, and its groups show
    /// among the names no customer covers in the add-on's state.
    Uncovered,
    /// Only an offboarded customer covers it: nothing of it is imported, and it is no conflict.
    Offboarded,
    /// Its groups are covered by two or more: it is not imported, and it is reported, with each
    /// of its groups that is covered, by name.
    Conflict(Vec<CoveredGroup>),
}

/// The host groups of Zabbix with the customer covering each.
#[derive(Debug)]
pub struct Coverage {
    /// The system's name as external references name it.
    system: String,
    /// The names of the list, each once ignoring case, with the one each is nested in.
    names: Vec<SystemName>,
    /// Who covers each name, by the name in lowercase.
    cover: HashMap<String, Option<Cover>>,
    /// Each group's name, by the group's id.
    groups: HashMap<String, String>,
}

impl Coverage {
    /// The coverage of `groups` by the external references in `system` (compared ignoring case)
    /// of the active `customers` and of the offboarded customers, `offboarded`.
    pub fn new(
        system: &str,
        groups: &[HostGroup],
        customers: &[Customer],
        offboarded: &[ExternalReference],
    ) -> Coverage {
        let lower_system = system.to_lowercase();
        let in_system = |r: &&ExternalReference| r.system.to_lowercase() == lower_system;
        let mut held: HashMap<String, Cover> = HashMap::new();
        for customer in customers {
            for reference in customer.external_references.iter().filter(in_system) {
                held.insert(reference.name.to_lowercase(), Cover::Customer(customer.id));
            }
        }
        for reference in offboarded.iter().filter(in_system) {
            held.insert(reference.name.to_lowercase(), Cover::Offboarded);
        }

        // Each name once ignoring case, the first of its spellings in name order kept.
        let mut sorted: Vec<&HostGroup> = groups.iter().collect();
        sorted.sort_by_cached_key(|g| (g.name.to_lowercase(), g.name.clone()));
        let mut spelled: HashMap<String, String> = HashMap::new();
        let mut kept: Vec<&str> = Vec::new();
        for group in &sorted {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                spelled.entry(group.name.to_lowercase())
            {
                entry.insert(group.name.clone());
                kept.push(&group.name);
            }
        }
        // Each name nested in the nearest name above it by `/` that the list has.
        let parent_of = |name: &str| -> Option<String> {
            let mut above = name;
            while let Some((head, _)) = above.rsplit_once('/') {
                if let Some(parent) = spelled.get(&head.to_lowercase()) {
                    return Some(parent.clone());
                }
                above = head;
            }
            None
        };
        let names: Vec<SystemName> = kept
            .iter()
            .map(|name| SystemName {
                name: (*name).to_owned(),
                parent: parent_of(name),
            })
            .collect();

        // Up from each name to the first one an external reference has; the list is in name
        // order, so a name's parent, a prefix of it, comes first and is already known.
        let mut cover: HashMap<String, Option<Cover>> = HashMap::new();
        for name in &names {
            let lower = name.name.to_lowercase();
            let covered = match held.get(&lower) {
                Some(holder) => Some(holder.clone()),
                None => name
                    .parent
                    .as_ref()
                    .and_then(|p| cover.get(&p.to_lowercase()).cloned().flatten()),
            };
            cover.insert(lower, covered);
        }
        Coverage {
            system: system.to_owned(),
            names,
            cover,
            groups: groups
                .iter()
                .map(|g| (g.id.clone(), g.name.clone()))
                .collect(),
        }
    }

    /// The name list of the add-on's system: every host group once ignoring case, nested in the
    /// nearest group its name is nested in by `/`, or at the top.
    pub fn name_list(&self) -> SystemNames {
        SystemNames {
            system: self.system.clone(),
            names: self.names.clone(),
        }
    }

    /// Who covers the group with this id; none when no external reference covers it or the group
    /// is unknown.
    pub fn of_group(&self, id: &str) -> Option<Cover> {
        let name = self.groups.get(id)?.to_lowercase();
        self.cover.get(&name).cloned().flatten()
    }

    /// The ids of the groups an active customer covers, whose hosts the add-on reads, in order.
    pub fn imported_groups(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .groups
            .keys()
            .filter(|id| matches!(self.of_group(id), Some(Cover::Customer(_))))
            .cloned()
            .collect();
        ids.sort();
        ids
    }

    /// Where a host in the groups with these ids goes.
    pub fn place<'a>(&self, group_ids: impl IntoIterator<Item = &'a str>) -> Placement {
        let covered: BTreeSet<CoveredGroup> = group_ids
            .into_iter()
            .filter_map(|id| Some(CoveredGroup::new(self.groups.get(id)?, self.of_group(id)?)))
            .collect();
        let covers: BTreeSet<&Cover> = covered.iter().map(|g| &g.cover).collect();
        match covers.into_iter().collect::<Vec<_>>().as_slice() {
            [] => Placement::Uncovered,
            [Cover::Customer(customer)] => Placement::Customer(*customer),
            [Cover::Offboarded] => Placement::Offboarded,
            _ => Placement::Conflict(covered.into_iter().collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BRIGHTFIELD: Uuid = Uuid::from_u128(0x0e7b_6a52_5d0f_4d5e_8a8e_3f2b_1c0d_9e8f);
    const HARBOR: Uuid = Uuid::from_u128(0x1f8c_7b63_6e10_4e6f_9b9f_4a3c_2d1e_0f90);

    fn groups(names: &[&str]) -> Vec<HostGroup> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| HostGroup {
                id: (100 + i).to_string(),
                name: (*name).to_owned(),
            })
            .collect()
    }

    fn customer(id: Uuid, name: &str, references: &[(&str, &str)]) -> Customer {
        Customer {
            id,
            name: name.to_owned(),
            is_help_desk: false,
            domains: vec![],
            external_references: references
                .iter()
                .map(|(system, name)| ExternalReference {
                    system: (*system).to_owned(),
                    name: (*name).to_owned(),
                })
                .collect(),
        }
    }

    fn reference(system: &str, name: &str) -> ExternalReference {
        ExternalReference {
            system: system.to_owned(),
            name: name.to_owned(),
        }
    }

    /// The id of the group with this name.
    fn id(groups: &[HostGroup], name: &str) -> String {
        groups.iter().find(|g| g.name == name).unwrap().id.clone()
    }

    #[test]
    fn groups_are_listed_nested_in_the_nearest_group_that_exists() {
        let groups = groups(&[
            "Brightfield/Offices/Floor 2",
            "Brightfield",
            "Harbor Lane/Docks/Crane",
            "Harbor Lane",
            "Zabbix servers",
            // Zabbix keeps case: the core takes a name once ignoring it.
            "zabbix servers",
        ]);
        let coverage = Coverage::new("Zabbix", &groups, &[], &[]);
        assert_eq!(
            coverage.name_list(),
            SystemNames {
                system: "Zabbix".into(),
                names: vec![
                    SystemName::top("Brightfield"),
                    // Brightfield/Offices does not exist: Floor 2 is nested in Brightfield.
                    SystemName::nested_in("Brightfield/Offices/Floor 2", "Brightfield"),
                    SystemName::top("Harbor Lane"),
                    SystemName::nested_in("Harbor Lane/Docks/Crane", "Harbor Lane"),
                    SystemName::top("Zabbix servers"),
                ],
            }
        );
    }

    /// The cases of the core's own coverage of a name list (core/src/name_lists.rs), so that the
    /// add-on imports what the add-on's state shows as covered.
    #[test]
    fn a_group_is_covered_by_its_own_reference_or_the_nearest_one_above() {
        let groups = groups(&[
            "Brightfield",
            "Brightfield/Offices",
            "Brightfield/Offices/Floor 2",
            "Brightfield/Docks",
            "Brightfield/Docks/Crane",
            "Brightfield/Archive",
            "Brightfield/Archive/Rack",
            "Unclaimed",
            "Unclaimed/Lab",
            "Unclaimed/Annex",
            "Elsewhere",
        ]);
        let customers = [
            customer(
                BRIGHTFIELD,
                "Brightfield",
                &[
                    // Systems and names compared ignoring case.
                    ("zabbix", "brightfield"),
                    // A subgroup of Brightfield's the same customer has too.
                    ("Zabbix", "Brightfield/Offices"),
                    // A subgroup of a name no customer has.
                    ("Zabbix", "Unclaimed/Lab"),
                    // A name of another system.
                    ("Ledger", "Elsewhere"),
                ],
            ),
            // A subgroup of Brightfield's another customer has.
            customer(HARBOR, "Harbor Lane", &[("Zabbix", "Brightfield/Docks")]),
        ];
        // A subgroup of Brightfield's a customer since offboarded has.
        let offboarded = [reference("Zabbix", "Brightfield/Archive")];
        let coverage = Coverage::new("Zabbix", &groups, &customers, &offboarded);

        let covering: Vec<(&str, Option<Cover>)> = groups
            .iter()
            .map(|g| (g.name.as_str(), coverage.of_group(&g.id)))
            .collect();
        let brightfield = Some(Cover::Customer(BRIGHTFIELD));
        let harbor = Some(Cover::Customer(HARBOR));
        assert_eq!(
            covering,
            [
                ("Brightfield", brightfield.clone()),
                ("Brightfield/Offices", brightfield.clone()),
                ("Brightfield/Offices/Floor 2", brightfield.clone()),
                ("Brightfield/Docks", harbor.clone()),
                ("Brightfield/Docks/Crane", harbor),
                ("Brightfield/Archive", Some(Cover::Offboarded)),
                ("Brightfield/Archive/Rack", Some(Cover::Offboarded)),
                ("Unclaimed", None),
                ("Unclaimed/Lab", brightfield),
                ("Unclaimed/Annex", None),
                ("Elsewhere", None),
            ]
        );
        let mut imported = coverage.imported_groups();
        imported.sort();
        let mut expected: Vec<String> = [
            "Brightfield",
            "Brightfield/Offices",
            "Brightfield/Offices/Floor 2",
            "Brightfield/Docks",
            "Brightfield/Docks/Crane",
            "Unclaimed/Lab",
        ]
        .iter()
        .map(|name| id(&groups, name))
        .collect();
        expected.sort();
        assert_eq!(imported, expected);
    }

    #[test]
    fn a_host_goes_to_the_one_customer_its_groups_are_covered_by() {
        let groups = groups(&[
            "Brightfield",
            "Brightfield/Offices",
            "Brightfield/Archive",
            "Harbor Lane",
            "Linux servers",
        ]);
        let customers = [
            customer(BRIGHTFIELD, "Brightfield", &[("Zabbix", "Brightfield")]),
            customer(HARBOR, "Harbor Lane", &[("Zabbix", "Harbor Lane")]),
        ];
        let offboarded = [reference("Zabbix", "Brightfield/Archive")];
        let coverage = Coverage::new("Zabbix", &groups, &customers, &offboarded);
        let place = |names: &[&str]| {
            let ids: Vec<String> = names.iter().map(|n| id(&groups, n)).collect();
            coverage.place(ids.iter().map(String::as_str))
        };

        // Two groups of one customer, and a group no customer has beside one it has.
        assert_eq!(
            place(&["Brightfield", "Brightfield/Offices"]),
            Placement::Customer(BRIGHTFIELD)
        );
        assert_eq!(
            place(&["Linux servers", "Harbor Lane"]),
            Placement::Customer(HARBOR)
        );
        assert_eq!(place(&["Linux servers"]), Placement::Uncovered);
        assert_eq!(place(&[]), Placement::Uncovered);
        assert_eq!(place(&["Brightfield/Archive"]), Placement::Offboarded);
        // Two customers, or a customer and an offboarded one.
        assert_eq!(
            place(&["Linux servers", "Harbor Lane", "Brightfield/Offices"]),
            Placement::Conflict(vec![
                CoveredGroup::new("Brightfield/Offices", Cover::Customer(BRIGHTFIELD)),
                CoveredGroup::new("Harbor Lane", Cover::Customer(HARBOR)),
            ])
        );
        assert_eq!(
            place(&["Brightfield/Archive", "Brightfield"]),
            Placement::Conflict(vec![
                CoveredGroup::new("Brightfield", Cover::Customer(BRIGHTFIELD)),
                CoveredGroup::new("Brightfield/Archive", Cover::Offboarded),
            ])
        );
        // A group the add-on did not read, such as one created meanwhile.
        assert_eq!(
            coverage.place(["999", &id(&groups, "Harbor Lane")]),
            Placement::Customer(HARBOR)
        );
    }

    #[test]
    fn a_group_spelled_twice_ignoring_case_is_covered_as_the_one_listed() {
        let groups = groups(&["Brightfield", "BRIGHTFIELD/Offices", "brightfield"]);
        let customers = [customer(
            BRIGHTFIELD,
            "Brightfield",
            &[("Zabbix", "Brightfield")],
        )];
        let coverage = Coverage::new("Zabbix", &groups, &customers, &[]);
        for group in &groups {
            assert_eq!(
                coverage.of_group(&group.id),
                Some(Cover::Customer(BRIGHTFIELD)),
                "{}",
                group.name
            );
        }
        assert_eq!(
            coverage.name_list().names,
            [
                SystemName::top("Brightfield"),
                SystemName::nested_in("BRIGHTFIELD/Offices", "Brightfield"),
            ]
        );
    }
}
