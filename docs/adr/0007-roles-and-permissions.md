# Agents have one role; the product defines the permissions, the help desk the roles

Every agent has exactly one role, and a role is a named set of permissions. The list of permissions
is fixed by the product, because each one is a check in the core's code; the help desk creates,
names and changes roles as it likes, from the UI or the command line. Reading (tickets, the inbox,
customers, assets, services) needs no permission: every agent sees every customer.

The Administrator role is built in. It stores no list: it allows every permission, including those
a later version adds, so upgrading never leaves the help desk without someone who can use a new
feature. It can be neither changed nor removed, and the core refuses any change that would leave
no active agent with it. The command line, which needs access to the server and its database, can
always give an agent the Administrator role, so the help desk cannot lock itself out.

An agent cannot hand out more than they hold. Giving a role, creating or changing one, and acting
on an agent (changing their role, resetting their password, deactivating them) all need the acting
agent to hold every permission involved; only an Administrator acts on Administrators. Without this
rule, "manage agents" or "manage roles" would amount to the Administrator role.

The core loads the agent's role with the session on every request, so a change applies at once and
a deactivated agent's sessions stop working even before they are deleted.

Permissions apply to every customer. Limiting a role to some customers (an outside technician who
sees only one customer's tickets) is left for later; it would be a list of customers on the role,
with every list and search of the core narrowed by it.

## Considered options

- **An administrator flag on the agent.** Simple, but the help desk asked to define who does what,
  such as agents who sort the inbox but do not reply.
- **Several roles per agent, adding up.** More flexible, but what an agent may do is then spread
  over several roles; with one role it is read in one place, and a new combination is a new role.
- **Permissions defined by the help desk.** A permission nobody's code checks would do nothing.
- **Storing the Administrator's permissions like any other role's.** An upgrade that adds a
  permission would then leave nobody holding it.
