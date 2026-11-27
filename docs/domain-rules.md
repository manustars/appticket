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
- A requester who has not departed has one or more e-mail addresses, and each address belongs to
  exactly one requester; a message from any of them identifies the requester. A reply goes to the
  address the requester last wrote from on that ticket; on a ticket they have not written on, the
  agent picks one, and later replies there go to it unless the agent picks another.
- A requester also has a name, phone numbers, a job title and internal notes; agents who manage
  customers correct them.
- A requester who leaves their customer is departed: their history stays, no ticket is opened for
  them, and every message from their addresses, replies included, waits in the inbox. The assets
  they hold lose their holder, and they stop being a key requester. A departed requester can come
  back, once they have an address of their own.
- A person who leaves one customer for another is not moved: they are departed at the first, with
  their history, and a new requester of the second. An address they keep passes from the departed
  requester to the new one, even when it is the last they had.
- Two requesters of the same customer who are one person are merged: the addresses, tickets and
  held assets of one pass to the other, which remains. A merge cannot be undone, and requesters of
  different customers are never merged.
- A requester identified with the wrong customer is moved, from their own page or by moving one of
  their tickets, under the rule for moving tickets below: all their tickets move with them. The
  assets they hold stay with their customer and lose their holder, they leave their site, and they
  stop being a key requester.
- A customer has any number of key requesters, who are requesters of that customer and not
  departed. They receive the messages about failures that affect their customer (the
  announcements sent from a parent ticket, and the message sent when its children are resolved)
  and the replies on its tickets that have no requester. These go to the key requester's contact
  address: one of their addresses, chosen by the agent who marks them as a key requester and
  changed by agents who manage customers; it is always one of their addresses.
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
  add-ons, and view the state of the mail and of signing in. The help desk defines the roles.
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

## Signing in

Decided in [ADR 0008](adr/0008-identity-providers-authenticate-the-product-authorizes.md).

- An agent signs in with a local password, or through an identity provider: at most one OpenID
  Connect provider and one LDAP directory per installation. The identity provider says who the
  agent is and whether they may enter; what they may do is always their role in the product.
- Signing in through an identity provider needs, when an authorized group is configured, that the
  agent belongs to it, checked at every sign-in (nested groups count in Active Directory). Someone
  in the group who is not an agent yet becomes one, with the default role; without an authorized
  group, only existing agents can sign in. A deactivated agent cannot sign in through an identity
  provider either, and signing in never reactivates or recreates them.
- An identity is linked to an existing agent by e-mail the first time, only when the identity
  provider says the address is verified; from then on by the identity provider's stable
  identifier. A linked agent's name and e-mail come from the identity provider at every sign-in
  and are not edited in the product; a sign-in whose e-mail belongs to another agent is refused.
- Agents created through an identity provider have no local password; existing agents keep
  theirs. Local sign-in can be switched off for the installation, which ends every session opened
  with a local password; the command line can always switch it back on and set a password.
  Whether local sign-in is on is kept by the product, not in the deployment, and changed with the
  command line, so that switching it back on needs nothing but the command line. It cannot be
  switched off while no identity provider is configured.
- A password is checked by one system only: a linked agent's against the directory, an agent
  without a link against the local password, an unknown name against the directory.
- A session opened through an identity provider lasts 12 hours by default; one opened with a
  local password lasts 14 days. Signing out ends only the product's session.
- Repeated failed sign-ins, local or LDAP, are slowed and then refused for a while, per account
  and per address, before a directory's own lockout would trigger. The address is that of the
  connection; behind a reverse proxy it is taken from the forwarded headers only when the
  connection comes from a proxy the installation lists as trusted, so that a client cannot
  choose the address it is counted under. After a number of failures within a window (5 in 15
  minutes unless the installation says otherwise), for the account or from the address, the next
  four attempts wait 1, 2, 4 and 8 seconds before the password is checked; attempts after those
  are refused, without checking the password, until a window has passed since the last failure.
  So at most that number plus four passwords are checked per account within a window. A refused
  attempt is answered the same way whether or not the account exists, and is not counted. A
  password that could not be checked is no failure. Signing in clears the account's failures,
  not the address's. The account is what was typed in, trimmed and in lower case, whether or not
  an agent has it; an IPv6 address counts by its /64 network.
- Unlinking an agent's identity or removing their local password follows the rules for acting on
  an agent above, and is done by another agent: nobody changes how they sign in themselves.
  Neither leaves an agent without a sure way to sign in: an identity is unlinked only from an
  agent with a local password, and a local password is removed only from an agent linked to the
  identity provider configured now. Unlinking ends the sessions opened through the identity
  provider; removing the local password ends the sessions opened with it.

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
  and requester. Resolving the children of a parent sends a message to each child's requester and to
  the key requesters of each child's customer, each person once, and resolves every child that is
  open or waiting; the parent itself is resolved separately. Before that, at any time and as often
  as needed, an agent who can reply to requesters sends an announcement from a parent that is not
  closed: it goes to the key requesters of its children's customers and of the customers that use
  the services the parent links. This is the only link between tickets of different customers.
  Parents are one level deep, a parent stays with the help desk's own organization, and closed
  tickets are neither attached nor detached.

## Customers, assets and services

- Exactly one customer is the help desk's own organization, and it uses every service the help desk
  provides.
- Customers are offboarded, never deleted. Every message from an offboarded customer's requesters
  or domains, replies included, waits in the inbox, and its sites and requesters do not change. An
  offboarded customer can come back, with its history, requesters and assets.
- A customer has a name, and optionally a legal name, a tax identifier with its country, an
  address, a phone number, a website and internal notes.
- A customer has any number of external references, each made of a system and a name in it; a
  customer can have several in the same system, and each one names exactly one customer.
- A customer has any number of sites, each with a name and an address. A customer's active sites
  have different names, ignoring case. Sites are retired, never deleted: a retired site gets no
  new requesters or assets, and those at it are left without a site after a warning. A retired
  site can still be corrected.
- An asset belongs to exactly one customer.
- Every asset has an asset kind from the list the help desk manages; the kind says whether its
  assets move with their holder or stay at their site. No two asset kinds have names that differ
  only in case. An add-on that supplies assets maps its own kinds to that list in its
  configuration.
- An asset has optionally a serial number, a site of its customer and a holder, who is a requester
  of its customer and not departed. An asset whose kind moves with its holder is at its holder's
  site while it has a holder, and at its own site otherwise; any other asset is at its own site.
- A ticket that is not closed is at the sites of the assets it links, or, when it links none, at
  its requester's site; a closed ticket stays at the sites it was at when it was closed.
- A service is linked explicitly to the customers that use it, and optionally to the assets it runs
  on, which can belong to any customer.
- Assets and services are retired, never deleted: a retired one stays on the tickets that refer to
  it but gets no new links. A retired asset loses its holder.
- A customer stops using a service only once none of its tickets refers to the service; the help
  desk's own organization never stops.
- The links of a closed ticket do not change.

## Importing

- Customers, sites, requesters and assets can be imported from CSV files, one file per kind with
  fixed column names; several files in one import are taken in that order, so that a row can refer
  to something created by an earlier file.
- An import recognises what exists already: a customer by external reference, else by name; a
  requester by any of their addresses; an asset by serial number within its customer, else by name
  and customer. Sites, holders and kinds are named and must exist. A filled cell sets a value, an
  empty one leaves it; an import never deletes or retires anything, and never changes an asset an
  add-on is the source of.
- Rows that break a rule are left out with their reason, together with the rows that depend on
  them; the other rows are imported. Every import first shows what it would create, update and
  leave out, and applies only when asked; the rows left out come back as a file with their reason,
  ready to correct and import again.
- Importing needs managing customers, and managing the catalog for assets.

## Add-ons

- An add-on changes only what it is the source of: the assets it supplied (their name, kind,
  serial number and site, and retiring them when they leave its system), never a holder, an asset
  entered by hand or one another add-on supplied. Customers and requesters supplied by add-ons
  will follow the same rule.
- An add-on knows each asset it supplies by a reference of its own, which names one asset of one
  customer for good: supplying an asset with a reference the add-on gave before updates that asset,
  and brings it back if the add-on retired it; nothing else brings back a retired asset. It names
  the kind from the help desk's list and the site from the customer's active sites, and changes
  nothing of an offboarded customer. Agents give a supplied asset a holder, which the add-on
  supplying it again leaves, but change nothing else its source supplied, nor retire it.
- An asset has at most one source and can be linked to other add-ons, which only supply live
  details and links. When an add-on would create an asset and its customer already has an active
  one with the same serial number or name (ignoring case), entered by hand or supplied by another
  add-on, the core records a link proposal instead of a second asset; the serial number decides
  over the name. A reference of an add-on has at most one open link proposal. Every agent sees it
  on the customer's assets; accepting or discarding it needs managing the catalog, and once it is
  discarded the add-on's next attempt with that reference creates its own asset.
- Accepting a link proposal links the add-on to that asset, if it is still active, with its
  reference; an add-on is linked to an asset at most once, and never to one it is the source of.
  From then on the reference reaches that asset, and any change the add-on sends to its kind,
  name, serial number or site is refused (names and serial numbers compared ignoring case). A linked add-on unlinks itself when the asset leaves its
  system, which never retires the asset; an asset lists its source and its linked add-ons.
- Agents who manage customers set a customer's external references in an add-on's system,
  choosing from the add-on's name list; the add-on only reads them. The add-on replaces its whole
  name list, with the nesting of its names (each name once ignoring case, nested in another of
  the list or at the top, never in itself), under its system's name; the core keeps the latest
  list, also while the add-on is switched off, and a system's names come from one add-on. In that
  system a new external reference, typed or imported, is one of the listed names, spelled as the
  list spells it and its system, and a name that is already an external reference of a customer
  cannot be picked for another. Replacing the list changes no external reference: those typed by
  hand before, and those whose name left the list, stay. A list sent under another system's name
  renames nothing: the references picked before stay under the old name, which no add-on lists
  any more, so names can be typed there again, and the add-on no longer finds them under its
  system. An external reference covers the names nested in it too, unless one of them is an
  external reference of another customer, which then covers it and the names nested in it.
  Anything of the external system that no external reference covers, or that two cover, is not
  imported: the add-on reports it as a conflict, with a sentence that those who manage add-ons can
  act on, replacing its whole list of conflicts. The add-on's state, which only those who manage
  add-ons read, shows the latest list, also while the add-on is switched off, and the names of its
  latest name list that no external reference covers. A name an offboarded customer has is
  covered, and so are the names nested in it, even inside the name of an active customer: they
  are not listed, and nothing of them is imported for either customer.
- The live details of an asset come from its source and each add-on it is linked to, asked with
  that add-on's reference when the asset is shown, and only for an active asset of a customer
  that is not offboarded; when an add-on does not answer within 2 seconds, answers with an error
  or is switched off, they are unavailable, and nothing else changes or waits for it.
- An add-on allowed to work tickets opens tickets without a requester for a customer, with a
  priority, assets and services; it adds notes to any ticket and resolves the tickets it opened,
  when they are open or waiting. It never replies and changes nothing else on tickets. It reads
  whether an agent has worked on a ticket, never the notes themselves: an agent has once they
  have been its assignee, written a reply, a note or an announcement on it, or changed it by hand:
  its state (even to the one it had, as when attaching an inbox message to it), priority, links or
  customer, or a child attached to or detached from it, or it to or from a parent. It stays so,
  also once they are unassigned. Notes of add-ons and messages received are no work of an agent.
- An add-on can give a ticket it opens a reference of its own, such as the id of the problem of
  its system the ticket is about, which names that ticket for good: nothing adds, changes or
  removes it later. Opening a ticket again with a reference the add-on gave before opens no second
  ticket, also when two opens come at once: the add-on gets the ticket opened then, in any state,
  closed included, and nothing on it changes, whatever else it sends. A reference names one ticket
  of one add-on, and another add-on's references are its own. Only the add-on that gave it reads
  it, on the ticket and in the changes of its state it is told of; agents and other add-ons do
  not.
- Add-ons registered for `ticket.state_changed` are told of every change of a ticket's state,
  once, with the state it left, the state it reached and who changed it: an agent, a received
  message that reopened it, or the add-on that resolved a ticket it opened. A move to the state
  the ticket is already in is no change.
- An alert opens a ticket of the customer of the asset it is about, only when the monitoring
  add-on's rules say so, and at most one ticket per problem of the monitoring system: the add-on
  gives the ticket the problem's id as its reference. When the problem recovers, the add-on adds a
  note and resolves the ticket only if no agent has worked on it (as above), unless its rule says
  otherwise.

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
