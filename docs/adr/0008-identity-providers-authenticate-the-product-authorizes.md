# Identity providers say who an agent is; the product decides what they may do

Agents can sign in through an identity provider: at most one OpenID Connect provider and one LDAP
directory per installation, configured with the deployment (environment and chart values, secrets
in Secrets) like the mail. The identity provider authenticates the agent and, through an
authorized group checked at every sign-in, says whether they may enter; someone in that group
signing in for the first time becomes an agent with the default role. What an agent may do stays
their role in the product: no group of the identity provider maps to a role.

An identity is linked to an existing agent by verified e-mail once, then by the provider's stable
identifier, and the linked agent's name and e-mail follow the provider. Local passwords stay as a
fallback that the installation can switch off; the command line can always switch them back on,
so an unreachable identity provider never locks the help desk out. Sessions opened through an
identity provider last a working day, so that an account disabled there loses access within
hours. Repeated failed sign-ins are refused before a directory's own lockout would trigger, so that
guessing at the product's sign-in page cannot lock a person out of the rest of their organization.

## Considered options

- **Roles from the identity provider's groups.** Saves managing roles twice, but whoever manages
  groups in the identity provider would hold the Administrator role in practice, around the rule
  that an agent cannot hand out more than they hold (ADR 0007).
- **Only agents created by hand can sign in.** Safest, but every new colleague waits for an
  administrator; the authorized group gives the same control from the identity provider, and
  leaving it unset keeps this behaviour.
- **Letting anyone in the identity provider sign in.** In an organization's directory that would
  show every customer's tickets to every employee.
- **Linking by e-mail at every sign-in.** Breaks when an address changes, and an unverified address
  would let someone take over another agent's account.
- **Identity providers configured in the UI.** Their secrets would live in the database and be
  changeable by any agent who manages the installation; the deployment is already where mail
  secrets live.
- **Several OpenID Connect providers at once.** Agents are the help desk's own staff, usually in one
  organization; more providers can be added later without changing this decision.
