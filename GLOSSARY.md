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
_Avoid_: Dismissed, deleted, archived, inactive

**Requester**:
A person, belonging to exactly one customer, who asks the help desk for assistance.
_Avoid_: User, contact, end user

**Agent**:
A member of the help desk's staff who handles tickets.
_Avoid_: Operator, technician, user

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

**Assignee**:
The one agent responsible for a ticket at a given time.
_Avoid_: Owner, handler

**Message**:
An e-mail the help desk receives or sends; once attached to a ticket, it is part of that ticket's
conversation with its requester.
_Avoid_: Mail, e-mail

**Reply**:
A message an agent writes to the requester of a ticket; it is sent through the outbox.
_Avoid_: Answer, response

**Outbox**:
Where a reply an agent has written waits until it is sent to the requester.
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

**Service**:
Something the help desk provides to one or more customers, such as hosted mail, backup or
monitoring.
_Avoid_: Subscription, product, offering

## AI

**Assistant**:
The AI add-on that reads incoming messages and produces proposals.
_Avoid_: AI agent, bot

**Proposal**:
A course of action the Assistant suggests, which takes effect only once an agent accepts it.
_Avoid_: Suggestion, recommendation
