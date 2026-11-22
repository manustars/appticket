-- Roles, the permissions they hold, and deactivated agents (ADR 0007).

CREATE TABLE roles (
    id                uuid PRIMARY KEY,
    name              text NOT NULL CHECK (btrim(name) <> ''),
    -- The Administrator role allows every permission, including ones later versions add, so it
    -- stores none.
    is_administrator  boolean NOT NULL DEFAULT false,
    -- The role new agents get when none is named.
    is_default        boolean NOT NULL DEFAULT false,
    -- The permissions the product defines (core/src/roles.rs); a new one needs a migration.
    permissions       text[] NOT NULL DEFAULT '{}' CHECK (permissions <@ ARRAY[
        'tickets.work', 'tickets.reply', 'tickets.open', 'tickets.assign', 'tickets.move',
        'inbox.triage', 'customers.manage', 'catalog.manage', 'agents.manage', 'roles.manage',
        'addons.manage', 'mail.view'
    ]::text[]),
    created_at        timestamptz NOT NULL DEFAULT now(),
    CHECK (NOT is_administrator OR (permissions = '{}' AND NOT is_default))
);

CREATE UNIQUE INDEX roles_name ON roles (lower(name));
CREATE UNIQUE INDEX roles_one_administrator ON roles (is_administrator) WHERE is_administrator;
CREATE UNIQUE INDEX roles_one_default ON roles (is_default) WHERE is_default;

INSERT INTO roles (id, name, is_administrator) VALUES (gen_random_uuid(), 'Administrator', true);
INSERT INTO roles (id, name, is_default, permissions) VALUES (gen_random_uuid(), 'Agent', true,
    ARRAY['tickets.work', 'tickets.reply', 'tickets.open', 'tickets.assign', 'tickets.move',
          'inbox.triage']);

-- The agents there are already keep everything they could do.
ALTER TABLE agents
    ADD COLUMN role_id uuid REFERENCES roles (id),
    -- Set when the agent leaves the help desk: they can no longer sign in or be assigned.
    ADD COLUMN deactivated_at timestamptz;
UPDATE agents SET role_id = (SELECT id FROM roles WHERE is_administrator);
ALTER TABLE agents ALTER COLUMN role_id SET NOT NULL;
CREATE INDEX agents_role ON agents (role_id);
