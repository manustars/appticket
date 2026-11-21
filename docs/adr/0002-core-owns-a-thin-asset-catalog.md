# The core owns a thin asset catalog that add-ons can feed

A ticket refers to zero or more assets, and those assets live in a catalog the core owns: each
asset has only a customer, a kind and a name, plus an optional source (the add-on it came from and
that add-on's identifier for it). Agents can create assets by hand; an add-on such as an inventory
or monitoring integration can create them and supply further details. When that add-on is switched
off, every asset and every link from a ticket survives, and only the add-on's details become
unavailable.

## Considered options

- **A full catalog in the core.** Survives any add-on being off, but the core would have to do what
  inventory tools already do better, and synchronised data would exist in two places.
- **Only references supplied by add-ons.** Keeps the core small, but a customer without an add-on
  could not link anything to a ticket (the common case for a help desk with many small customers),
  and switching an add-on off would leave tickets pointing at little more than a label.
