# Services are distinct from assets

An asset belongs to exactly one customer, and a ticket links only assets of its own customer. What
the help desk itself provides to several customers at once (hosted mail, backup, monitoring) is a
Service, linked explicitly to the customers that use it; a ticket links only services its customer
uses. A service may be linked to the assets it runs on: usually assets of the help desk's own
organization (see ADR 0001), though a managed service can run on a customer's asset.
The explicit "customer uses service" link also answers which customers to inform when a service
fails.

## Considered options

- **Let any ticket link any asset.** Simpler, but it loses the information about which customers are
  involved, and a constraint dropped now is costly to bring back once the data no longer obeys it.
- **Let an asset belong to several customers.** Blurs "what the customer owns" with "what the help
  desk provides", which behave differently: one customer is affected by an asset, many by a service.
