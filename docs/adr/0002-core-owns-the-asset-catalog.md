# The core owns the asset catalog; add-ons feed it and supply live details

A ticket refers to zero or more assets, and those assets live in a catalog the core owns. Each
asset has what the help desk needs to work without any add-on: its customer, an asset kind from a
list the help desk manages, a name, and optionally a serial number, a site and a holder, plus an
optional source (the add-on it came from and that add-on's identifier for it). Agents can create
assets by hand; an add-on such as an inventory or monitoring integration can create and update
them, and supplies the live details the core does not keep: whether a device is online, its
addresses, its open alerts. When that add-on is switched off, every asset and every link from a
ticket survives, and only the add-on's live details become unavailable.

An asset has at most one source, which alone changes what it supplied, and can be linked to other
add-ons as well: a server monitored by one integration and reached through another is one asset,
with live details and links from both. An add-on that finds an asset it would supply already in the
catalog, entered by hand or by another add-on, proposes the link to the agents instead of creating
a second one.

_Revised on 2026-11-22._ The first version kept only a customer, a kind in free text and a name, so
that the core would not duplicate inventory tools. Working out the customers' data showed that
where an asset is and who uses it decide where a ticket is, and a help desk without an inventory
add-on still needs them; a free-text kind could not say whether an asset moves with its holder.

## Considered options

- **A full inventory in the core.** Survives any add-on being off, but the core would have to do
  what inventory and monitoring tools already do better, and data that changes by the minute would
  exist in two places.
- **Only references supplied by add-ons.** Keeps the core small, but a customer without an add-on
  could not link anything to a ticket (the common case for a help desk with many small customers),
  and switching an add-on off would leave tickets pointing at little more than a label.
- **One add-on per asset.** Simpler, but the same server would appear twice, once per
  integration, and a ticket would have to link both to show its state and reach it remotely.
- **Letting every linked add-on write the asset.** Two integrations would overwrite each other's
  serial number and name; one source per asset keeps one answer.
- **Keeping the catalog as thin as first decided.** Sites and holders would then live only in
  add-ons, and a help desk without one could not tell which office a ticket concerns.
