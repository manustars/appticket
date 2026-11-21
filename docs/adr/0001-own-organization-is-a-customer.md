# The help desk's own organization is a customer like any other

The help desk gives IT assistance both to its own organization and to external organizations. Rather
than an "internal" mode plus a free-text customer field on tickets, every organization the help
desk serves is a Customer, its own included, and every requester belongs to exactly one customer.
Exactly one customer is marked as the help desk's own organization: it uses every service the
help desk provides and owns parent tickets and monitoring alerts. In every other respect it is
treated like any other customer, so internal assistance needs no mode of its own.

## Considered options

- **A free-text customer field on the ticket.** Enough to filter, but it cannot say which customer a
  requester, or anything a ticket refers to, belongs to, and it would have to be replaced by an entity as soon as
  anything else hangs off the customer.
- **Internal and external as two modes of the product.** Doubles every rule that touches customers
  for no gain.
