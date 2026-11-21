# Domain rules

Rules agreed with the maintainer that the ticket schema must respect. Terms are defined in
[`GLOSSARY.md`](../GLOSSARY.md); decisions with their reasons are in [`adr/`](adr/).

## Incoming messages

- A reply to an existing ticket attaches to that ticket, whoever sends it: replies are recognised
  by their mail headers, not by the sender.
- A new message from a known requester opens a new ticket.
- A message from an unidentified sender waits in the inbox, where an agent opens a ticket for it,
  attaches it to an existing ticket or discards it.
- The sender is identified first by address (a registered requester), then by domain (a domain
  registered on a customer, which creates a new requester of that customer); otherwise the sender
  is unidentified.
- E-mail is the only automatic channel; an agent can also open a ticket by hand.

## People

- Every requester belongs to exactly one customer.
- Requesters have no access to the product for now: they only exchange messages.
- Agents are always staff of the help desk and see every customer.
- A ticket has at most one assignee; other agents can still write messages and notes on it.

## Tickets

- Every ticket belongs to exactly one customer; the requester is optional (planned work, monitoring
  alerts) and, when present, belongs to that customer.
- A ticket links zero or more assets of its own customer and zero or more services its customer
  uses, at any time.
- A ticket's states are open, waiting, resolved (a reply from the requester reopens it) and closed
  (final; a new message opens a new ticket).
- A ticket can be moved to another customer. Linked assets and services that do not fit the new
  customer are unlinked after a warning, and the agent either moves the requester too or removes the
  requester from the ticket. No ticket is ever left breaking the rules above.
- A parent ticket belongs to the help desk's own organization; its children keep their own customer
  and requester. Resolving the parent lets an agent resolve every child with one message to each
  requester. This is the only link between tickets of different customers.

## Customers, assets and services

- Exactly one customer is the help desk's own organization, and it uses every service the help desk
  provides.
- Customers are offboarded, never deleted. Every message from an offboarded customer's requesters
  or domains, replies included, waits in the inbox.
- An asset belongs to exactly one customer.
- A service is linked explicitly to the customers that use it, and optionally to the assets it runs
  on, which can belong to any customer.

## Assistant

- It proposes on inbox messages (open a ticket for a customer, attach to a ticket, discard) and on
  tickets (assets involved, priority, a draft reply).
- It never acts alone: every proposal waits for an agent to accept, edit or discard it.

## Out of scope for now

SLAs, contracts and billing; a web form or API for requesters; custom ticket states.
