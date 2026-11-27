# appticket

A product for IT assistance: it turns the messages a help desk receives into tickets, with an
optional AI add-on that reads each message and proposes what to do with it.

## Product

**Core**:
The part of the product that is always present and owns the help desk's own data.

**Add-on**:
A separate program that extends the core and that the core can switch on and off; it may keep data
of its own, and switching it off never makes the core's own data unusable.
_Avoid_: Plugin, extension, module

## Organizations and people

**Help desk**:
The organization that gives IT assistance; its staff are the agents.
_Avoid_: Provider, MSP, vendor

**Customer**:
An organization the help desk gives assistance to; the help desk's own organization is one of them.
_Avoid_: Client, account, tenant

**Offboarded**:
Said of a customer the help desk no longer serves; its history stays, but it gets no new tickets.
An offboarded customer can come back.
_Avoid_: Dismissed, deleted, archived, inactive

**Site**:
A place where a customer works, such as an office or a data centre; a customer has any number of
them, and its requesters and assets can each be at one.
_Avoid_: Location, branch, office

**External reference**:
A name another system uses for a customer, such as a group in a monitoring tool; a customer can
have several in the same system, and each one names exactly one customer.
_Avoid_: External ID, external code, mapping

**Requester**:
A person, belonging to exactly one customer, who asks the help desk for assistance.
_Avoid_: User, contact, end user

**Key requester**:
A requester who speaks for their customer: they receive the announcements about failures that
affect it and the replies on its tickets that have no requester; a customer has any number of them.
_Avoid_: Contact, point of contact, referent

**Departed**:
Said of a requester who no longer works for their customer; their history stays, but no new
ticket is opened for them and their messages wait in the inbox. A departed requester can come back.
_Avoid_: Deleted, offboarded, deactivated, inactive

**Agent**:
A member of the help desk's staff who handles tickets.
_Avoid_: Operator, technician, user

**Identity provider**:
A system outside the product that confirms who an agent is when they sign in, such as the
organization's sign-in service or its directory; it never decides what the agent may do.
_Avoid_: SSO, IdP, directory, authentication server

**Deactivated**:
Said of an agent who no longer works at the help desk; their history stays, but they cannot sign in
and no ticket can be assigned to them.
_Avoid_: Deleted, offboarded, disabled, suspended

**Role**:
A named set of permissions the help desk defines; every agent has exactly one.
_Avoid_: Group, profile, access level

**Permission**:
Something beyond reading that a role allows, such as replying to requesters or managing customers;
the product defines which permissions exist.
_Avoid_: Right, privilege, grant, scope

**Administrator**:
The role that allows everything the product can do, now and in later versions; it can be neither
changed nor removed.
_Avoid_: Superuser, root, owner, admin

## Work

**Ticket**:
A unit of assistance the help desk gives to a customer, usually because a requester asked for it;
it is open, waiting, resolved or closed.
_Avoid_: Request, case, issue

**Open** / **Waiting** / **Resolved** / **Closed**:
The states of a ticket: someone has to act on it; the help desk awaits the requester; an agent
considers the work done, though a reply from the requester reopens it; finished for good.

**Parent ticket** / **Child ticket**:
A ticket of the help desk's own organization that tracks one failure affecting several customers,
and each customer's ticket attached to it.
_Avoid_: Major incident, problem, master ticket

**Priority**:
How soon a ticket needs attention: low, normal, high or urgent.
_Avoid_: Severity, urgency, importance

**Assignee**:
The one agent responsible for a ticket at a given time.
_Avoid_: Owner, handler

**Message**:
An e-mail the help desk receives or sends; once attached to a ticket, it is part of that ticket's
conversation with its requester.
_Avoid_: Mail, e-mail

**Reply**:
A message an agent writes to the requester of a ticket, or to the key requesters of its customer
on a ticket without one; it is sent through the outbox.
_Avoid_: Answer, response

**Outbox**:
Where a reply an agent has written waits until it is sent to the requester, or to the key
requesters.
_Avoid_: Queue, spool

**Inbox**:
Where incoming messages that no ticket has claimed wait for an agent to decide what to do with them.
_Avoid_: Queue, triage

**Note**:
An internal remark on a ticket, seen only by agents and never sent outside the help desk.
_Avoid_: Comment, internal message

## What tickets refer to

**Asset**:
Something a customer owns that the help desk looks after, such as a computer, an application or a
licence.
_Avoid_: Configuration item, CI, device, item, target

**Source**:
The add-on an asset comes from; only its source changes what it supplied, while other add-ons
linked to the asset only show their live details. The source identifies the asset by a reference
of its own, which is not an External reference (a customer's name in another system).
_Avoid_: Origin, owner, provider

**Alert**:
A problem that a monitoring add-on reports on an asset; it opens a ticket of the asset's customer
when that add-on's rules say so.
_Avoid_: Alarm, event, incident, notification

**Asset kind**:
A category of assets the help desk defines, such as laptop, desktop or printer; it says whether
its assets move with their holder or stay at their site.
_Avoid_: Type, category, class

**Holder**:
The requester an asset has been given to, such as the person who uses a laptop; an asset has at
most one, of its own customer.
_Avoid_: Assignee, owner, user

**Service**:
Something the help desk provides to one or more customers, such as hosted mail, backup or
monitoring.
_Avoid_: Subscription, product, offering

## AI

**Assistant**:
The AI add-on that reads incoming messages and produces proposals.
_Avoid_: AI agent, bot

**Proposal**:
A course of action the Assistant suggests on a message or a ticket, which takes effect only once an
agent accepts it.
_Avoid_: Suggestion, recommendation
