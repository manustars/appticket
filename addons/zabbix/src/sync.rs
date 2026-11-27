//! One round of the add-on's work: Zabbix's host groups go to the core as the name list of its
//! system, and the hosts of the groups customers cover become their assets; what left them is
//! retired, and the hosts of groups two customers cover are reported as conflicts and left as
//! they are.

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use hd_addon_sdk::Client;
use hd_addon_sdk::api::{Supplied, SuppliedAsset, SuppliedDetails};
use uuid::Uuid;

use crate::conflicts::{self, HostConflict};
use crate::coverage::{Coverage, Placement};
use crate::kinds::Kinds;
use crate::zabbix::{Host, Zabbix};

/// The add-on's own reference for the asset of a host of a customer: a reference names one asset
/// of one customer for good, so a host that moves to another customer becomes another asset.
pub fn reference(host_id: &str, customer: Uuid) -> String {
    format!("{host_id}@{customer}")
}

/// The host a reference of [`reference`] is of.
fn host_of(reference: &str) -> &str {
    reference
        .split_once('@')
        .map_or(reference, |(host, _)| host)
}

/// The add-on's work, between a Zabbix and the core, round after round.
#[derive(Debug)]
pub struct Syncer {
    core: Client,
    zabbix: Zabbix,
    /// The system's name as external references name it.
    system: String,
    kinds: Kinds,
}

/// What a round did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Round {
    /// Host groups listed.
    pub names: usize,
    /// Hosts that are assets of a customer, including those linked to an asset the customer had.
    pub hosts: usize,
    pub created: usize,
    pub updated: usize,
    /// Hosts whose asset waits as a link proposal for the agents.
    pub proposed: usize,
    pub retired: usize,
    pub unlinked: usize,
    pub conflicts: usize,
    /// Calls the core refused, each logged with its sentence.
    pub refused: usize,
}

/// An asset of a customer the add-on is the source of or linked to, by its reference.
struct Known {
    id: Uuid,
    /// Linked to it, rather than its source.
    linked: bool,
}

impl Syncer {
    pub fn new(core: Client, zabbix: Zabbix, system: impl Into<String>, kinds: Kinds) -> Self {
        Syncer {
            core,
            zabbix,
            system: system.into(),
            kinds,
        }
    }

    /// One round: the name list, the assets of the hosts customers cover, and the conflicts.
    /// Nothing is retired unless both Zabbix and the core answered every read.
    pub async fn run(&self) -> anyhow::Result<Round> {
        let mut round = Round::default();
        let me = self
            .core
            .whoami()
            .await
            .context("asking the core who the add-on is")?;
        let groups = self.zabbix.host_groups().await?;
        let customers = self
            .core
            .customers()
            .await
            .context("reading the customers")?;
        let offboarded = self
            .core
            .offboarded_references()
            .await
            .context("reading the offboarded customers' external references")?;
        let coverage = Coverage::new(&self.system, &groups, &customers, &offboarded);

        let list = coverage.name_list();
        round.names = list.names.len();
        if let Err(e) = self.core.replace_names(&list).await {
            // The references picked from the latest list the core took still count.
            tracing::error!(error = %e, "the core did not take the name list");
            round.refused += 1;
        }

        let hosts = self.zabbix.hosts(&coverage.imported_groups()).await?;
        let mut wanted: Vec<(Uuid, &Host)> = Vec::new();
        let mut found: Vec<HostConflict> = Vec::new();
        // A host in conflict is still covered: its asset, if it has one, is left as it is,
        // holder and all, until the host is in the groups of one customer again.
        let mut in_conflict: HashSet<&str> = HashSet::new();
        for host in &hosts {
            match coverage.place(host.group_ids.iter().map(String::as_str)) {
                Placement::Customer(customer) => wanted.push((customer, host)),
                Placement::Conflict(groups) => {
                    in_conflict.insert(&host.id);
                    found.push(HostConflict {
                        host: host.name.clone(),
                        groups,
                    });
                }
                Placement::Uncovered | Placement::Offboarded => {}
            }
        }

        // What the add-on supplied, or is linked to, of each customer it reads.
        let mut known: HashMap<String, Known> = HashMap::new();
        for customer in &customers {
            let assets = self
                .core
                .customer_assets(customer.id)
                .await
                .with_context(|| format!("reading the assets of {}", customer.name))?;
            for asset in assets {
                if let Some(reference) = asset.reference {
                    let linked = asset.source_addon.as_deref() != Some(me.name.as_str());
                    known.insert(
                        reference,
                        Known {
                            id: asset.id,
                            linked,
                        },
                    );
                }
            }
        }

        let mut present = HashSet::new();
        for (customer, host) in &wanted {
            let reference = reference(&host.id, *customer);
            present.insert(reference.clone());
            round.hosts += 1;
            // The agents linked the host to an asset the customer had, whose details are
            // theirs or another add-on's: it is left as it is.
            if known.get(&reference).is_some_and(|k| k.linked) {
                continue;
            }
            let asset = SuppliedAsset {
                reference: reference.clone(),
                customer_id: *customer,
                details: SuppliedDetails {
                    kind: self.kinds.kind_of(&host.traits).to_owned(),
                    name: host.name.clone(),
                    serial_number: host.serial_number.clone(),
                    site: None,
                },
            };
            match self.core.supply_asset(&asset).await {
                Ok(Supplied::Asset { created: true, .. }) => round.created += 1,
                // The reference reaches an asset the add-on is linked to that its source retired
                // (it is not among the active ones read above): the add-on unlinks itself, and
                // supplies the host as an asset of its own from the next round.
                Ok(Supplied::Asset { asset, .. })
                    if asset.source_addon.as_deref() != Some(me.name.as_str())
                        && asset.retired_at.is_some() =>
                {
                    self.unlink(asset.id, &reference, &mut round).await;
                }
                Ok(Supplied::Asset { .. }) => round.updated += 1,
                Ok(Supplied::LinkProposed(_)) => round.proposed += 1,
                Err(e) => {
                    tracing::warn!(host = %host.name, reference, error = %e, "the core did not take the host's asset");
                    round.refused += 1;
                }
            }
        }

        for (reference, asset) in &known {
            if present.contains(reference) || in_conflict.contains(host_of(reference)) {
                continue;
            }
            if asset.linked {
                self.unlink(asset.id, reference, &mut round).await;
                continue;
            }
            match self.core.retire_asset(asset.id).await {
                Ok(()) => round.retired += 1,
                Err(e) => {
                    tracing::warn!(reference, error = %e, "the core did not retire the asset");
                    round.refused += 1;
                }
            }
        }

        let names: HashMap<Uuid, String> =
            customers.iter().map(|c| (c.id, c.name.clone())).collect();
        found.sort_by(|a, b| a.host.cmp(&b.host));
        round.conflicts = found.len();
        if let Err(e) = self
            .core
            .replace_conflicts(&conflicts::report(&found, &names))
            .await
        {
            tracing::error!(error = %e, "the core did not take the list of conflicts");
            round.refused += 1;
        }
        Ok(round)
    }

    async fn unlink(&self, asset: Uuid, reference: &str, round: &mut Round) {
        match self.core.unlink_asset(asset).await {
            Ok(()) => round.unlinked += 1,
            Err(e) => {
                tracing::warn!(reference, error = %e, "the core did not unlink the add-on");
                round.refused += 1;
            }
        }
    }
}
