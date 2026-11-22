# Monitoring add-ons open tickets on their own, by explicit rules

An add-on that watches the customers' systems, such as a Zabbix integration, opens a ticket for an
alert by itself, without waiting for an agent to accept a proposal. Which alerts open a ticket is
decided by rules in the add-on's configuration: an ordered list in which the first rule that
matches decides, with conditions on severity, customer, tags and trigger, and an outcome of opening
a ticket (with a priority and possibly a service) or ignoring the alert. An alert no rule opens
leaves no ticket, and the default opens only the two highest severities. A command of the add-on
tells which rule a given alert would meet.

The ticket belongs to the customer of the asset the alert is about and has no requester, so its
replies go to that customer's key requesters. The add-on opens at most one ticket per problem of
its system, stops and reports when it would open more than a safety limit in a few minutes, adds a
note when the problem recovers, and resolves the ticket by itself only when no agent has worked on
it. It never writes to the customer: replies stay with the agents.

The Assistant keeps proposing only (ADR 0006): it interprets what people write, where a wrong
guess has a cost an agent must weigh, while an alert is a fact the monitored system has already
established.

## Considered options

- **Alerts as proposals, like the Assistant's.** Nothing happens without an agent, but an alert at
  night would wait until morning as a proposal, and the list of proposals would fill with what
  monitoring already decided.
- **Every alert opens a ticket.** Nothing is missed, but warnings and information alerts would bury
  the tickets that need someone.
- **Grouping alerts by host in the add-on.** Fewer tickets in a failure, but it duplicates what the
  monitoring system's own trigger dependencies are for; the safety limit covers the case where
  those are not set up.
- **Rules managed in the core's UI.** Easier to change, but a rules editor is a project of its own;
  a file can be reviewed and versioned, and the check command catches a wrong rule before it
  matters.
- **Alerts owned by the help desk's own organization** (the first version of ADR 0001). A ticket
  links only assets of its own customer, so it could not link the customer's server the alert is
  about.
