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
- E-mail is the only automatic channel; an agent can also open a ticket by hand, for a customer,
  with or without a requester (the same requester rules as for the inbox apply) and with an
  optional internal note recording what was said.

## People

- Every requester belongs to exactly one customer.
- A requester has one or more e-mail addresses, and each address belongs to exactly one requester;
  a message from any of them identifies the requester. A reply goes to the address the requester
  last wrote from on that ticket; on a ticket they have not written on, the agent picks one.
- A requester also has a name, phone numbers, a job title and internal notes; agents who manage
  customers correct them.
- A requester who leaves their customer is departed: their history stays, no ticket is opened for
  them, and every message from their addresses, replies included, waits in the inbox. The assets
  they hold lose their holder, and they stop being a key requester. A departed requester can come
  back.
- A customer has any number of key requesters, who are requesters of that customer and not
  departed. They receive the messages about failures that affect their customer (those sent when
  the children of a parent ticket are resolved) and the replies on its tickets that have no
  requester.
- Requesters have no access to the product for now: they only exchange messages.
- Agents are always staff of the help desk and see every customer.
- A ticket has at most one assignee; other agents can still write messages and notes on it.

## Agents, roles and permissions

- Every agent has exactly one role. Every signed-in agent can read tickets, the inbox, customers,
  assets and services; anything else needs a permission of the agent's role.
- The product defines the permissions: work tickets (notes, state, priority, links to assets and
  services, parent and child tickets, taking a ticket for oneself), reply to requesters, open
  tickets by hand, assign tickets to others, move tickets to another customer, sort the inbox,
  manage customers, manage the catalog (assets and services), manage agents, manage roles, manage
  add-ons, and view the state of the mail. The help desk defines the roles.
- Managing customers covers their sites, external references, requesters and key requesters;
  managing the catalog covers asset kinds and every detail of an asset, its holder included.
- Permissions apply to every customer.
- The Administrator role exists exactly once and allows everything, including permissions later
  versions add; it is never changed or removed.
- At least one agent who is not deactivated has the Administrator role at all times; a change that
  would leave none is refused. The command line can always give an agent the Administrator role.
- An agent cannot hand out more than they hold: a role can be given to an agent, created or changed
  only by an agent who holds every permission it has (before and after the change), and an agent's
  role can be changed, their password reset or the agent deactivated only by an agent who holds
  every permission of that agent's role. Only an Administrator acts on Administrators.
- One role, not the Administrator, is the default for new agents. The first agent of an
  installation gets the Administrator role.
- A role that some agent has cannot be removed, nor can the default role.
- Changes to a role or to an agent apply from the agent's next request, without signing in again.
- A deactivated agent cannot sign in, their sessions end at once and the tickets assigned to them
  that are not closed lose their assignee. No ticket can be assigned to a deactivated agent. A
  deactivated agent can be reactivated.
- Accepting a proposal needs the permission of what it does: sorting the inbox for an inbox
  message, working tickets for priority, assets and services, replying to requesters for a draft
  reply. Discarding one needs sorting the inbox or working tickets in the same way.
- Agents change their own password by giving the current one. An agent who manages agents resets
  another agent's password to a generated one, shown once. Either way the agent's other sessions
  end.

## Tickets

- A ticket has a priority: low, normal (the default), high or urgent. Agents change it, except on
  a closed ticket.

- Every ticket belongs to exactly one customer; the requester is optional (planned work, monitoring
  alerts) and, when present, belongs to that customer.
- A reply on a ticket with a requester goes to the requester; on a ticket without one, it goes to
  every key requester of the ticket's customer, and the ticket stays without a requester. A ticket
  with neither cannot be replied to. A reply goes through the outbox and moves the ticket to the
  state the agent picks: waiting, unless the agent says otherwise.
- What an agent writes on a ticket and has not sent yet is kept as their draft, one per agent and
  ticket, until they send it as a reply or a note; it survives leaving the page.
- A closed ticket changes no more: no state change, no assignee change, no reply, no message
  attached from the inbox. Agents can still add internal notes to it.
- Opening a ticket from an inbox message makes the sender its requester, registered with the
  ticket's customer if new, with the name the message carries; a sender who is already a requester of another customer is refused.
- A ticket links zero or more assets of its own customer and zero or more services its customer
  uses, at any time.
- A ticket's states are open, waiting, resolved (a reply from the requester reopens it) and closed
  (final; a new message opens a new ticket).
- A ticket that is not closed can be moved to another customer. Linked assets and services that do
  not fit the new customer are unlinked after a warning, and the agent either moves the requester
  too or removes the requester from the ticket. Moving the requester means the requester was
  identified with the wrong customer, so all of the requester's tickets move with them, closed ones
  included. No ticket is ever left breaking the rules above.
- A parent ticket belongs to the help desk's own organization; its children keep their own customer
  and requester. Resolving the children of a parent sends one message to each child's requester and
  resolves every child that is open or waiting; the parent itself is resolved separately. This is
  the only link between tickets of different customers. Parents are one level deep, a parent stays
  with the help desk's own organization, and closed tickets are neither attached nor detached.

## Customers, assets and services

- Exactly one customer is the help desk's own organization, and it uses every service the help desk
  provides.
- Customers are offboarded, never deleted. Every message from an offboarded customer's requesters
  or domains, replies included, waits in the inbox. An offboarded customer can come back, with its
  history, requesters and assets.
- A customer has a name, and optionally a legal name, a tax identifier with its country, an
  address, a phone number, a website and internal notes.
- A customer has any number of external references, each made of a system and a name in it; a
  customer can have several in the same system, and each one names exactly one customer.
- A customer has any number of sites, each with a name and an address.
- An asset belongs to exactly one customer.
- Every asset has an asset kind from the list the help desk manages; the kind says whether its
  assets move with their holder or stay at their site. An add-on that supplies assets maps its own
  kinds to that list in its configuration.
- An asset has optionally a serial number, a site of its customer and a holder, who is a requester
  of its customer and not departed. An asset whose kind moves with its holder is at its holder's
  site while it has a holder, and at its own site otherwise; any other asset is at its own site.
- A ticket is at the sites of the assets it links; when it links none, at its requester's site.
- A service is linked explicitly to the customers that use it, and optionally to the assets it runs
  on, which can belong to any customer.
- Assets and services are retired, never deleted: a retired one stays on the tickets that refer to
  it but gets no new links.
- A customer stops using a service only once none of its tickets refers to the service; the help
  desk's own organization never stops.
- The links of a closed ticket do not change.

## Assistant

- It proposes on inbox messages (open a ticket for a customer, attach to a ticket, discard) and on
  tickets (priority, assets and services involved, a draft reply).
- It never acts alone: every proposal waits for an agent to accept, edit or discard it. An
  accepted draft reply goes into the composer; the agent sends it.
- A proposal's rationale is written in every language of the agents' UI, and each agent reads it in
  the language they chose (English when theirs is missing). A draft reply is in the requester's
  language.
- A message or ticket has at most one open proposal of each kind; a newer one replaces it. A
  proposal that no longer applies (the message left the inbox, the ticket was closed) cannot be
  accepted.
- Its language model is a setting: an external service by default, or one on the help desk's own
  servers. Sending customers' mail to an external service is each help desk's data-protection
  decision, and the configuration says so.

## Out of scope for now

SLAs, contracts and billing; a web form or API for requesters; custom ticket states.
