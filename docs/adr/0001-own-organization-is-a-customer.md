# The help desk's own organization is a customer like any other

The help desk gives IT assistance both to its own organization and to external organizations. Rather
than an "internal" mode plus a free-text customer field on tickets, every organization the help
desk serves is a Customer, its own included, and every requester belongs to exactly one customer.
Exactly one customer is marked as the help desk's own organization: it uses every service the
help desk provides and owns parent tickets and the alerts on its own assets. In every other respect
it is treated like any other customer, so internal assistance needs no mode of its own.

_Revised on 2026-11-22._ The first version gave every monitoring alert to the help desk's own
organization. An alert concerns an asset, and a ticket links only assets of its own customer, so an
alert now opens a ticket of the asset's customer: on a customer's server, a ticket of that
customer; on the help desk's own infrastructure, a ticket of the help desk's own organization
([ADR 0010](0010-monitoring-add-ons-open-tickets-by-explicit-rules.md)).

## Considered options

- **A free-text customer field on the ticket.** Enough to filter, but it cannot say which customer a
  requester, or anything a ticket refers to, belongs to, and it would have to be replaced by an entity as soon as
  anything else hangs off the customer.
- **Internal and external as two modes of the product.** Doubles every rule that touches customers
  for no gain.
