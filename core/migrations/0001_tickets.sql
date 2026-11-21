-- Customers, requesters, agents, tickets and the messages that feed them.
-- Rules enforced here are listed in docs/domain-rules.md.

CREATE TABLE customers (
    id             uuid PRIMARY KEY,
    name           text NOT NULL CHECK (btrim(name) <> ''),
    -- Exactly one customer is the help desk's own organization (ADR 0001); the partial unique
    -- index below allows at most one, and setup creates it.
    is_help_desk   boolean NOT NULL DEFAULT false,
    offboarded_at  timestamptz,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX customers_one_help_desk ON customers (is_help_desk) WHERE is_help_desk;

-- Mail domains that identify a customer's requesters.
CREATE TABLE customer_domains (
    domain       text PRIMARY KEY CHECK (domain = lower(domain) AND domain <> ''),
    customer_id  uuid NOT NULL REFERENCES customers (id)
);

CREATE TABLE requesters (
    id           uuid PRIMARY KEY,
    customer_id  uuid NOT NULL REFERENCES customers (id),
    email        text NOT NULL UNIQUE CHECK (email = lower(email) AND email LIKE '%_@_%'),
    name         text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- Target of the composite key that keeps a ticket's requester inside the ticket's customer.
    UNIQUE (id, customer_id)
);

CREATE TABLE agents (
    id             uuid PRIMARY KEY,
    email          text NOT NULL UNIQUE CHECK (email = lower(email) AND email LIKE '%_@_%'),
    name           text NOT NULL CHECK (btrim(name) <> ''),
    password_hash  text NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE tickets (
    id            uuid PRIMARY KEY,
    -- The number agents and requesters see.
    number        bigint GENERATED ALWAYS AS IDENTITY UNIQUE,
    customer_id   uuid NOT NULL REFERENCES customers (id),
    requester_id  uuid,
    assignee_id   uuid REFERENCES agents (id),
    title         text NOT NULL,
    state         text NOT NULL DEFAULT 'open'
                  CHECK (state IN ('open', 'waiting', 'resolved', 'closed')),
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    -- The requester, when there is one, belongs to the ticket's customer.
    FOREIGN KEY (requester_id, customer_id) REFERENCES requesters (id, customer_id)
);

CREATE INDEX tickets_customer ON tickets (customer_id);

-- Every e-mail the help desk receives or sends. A received message with no ticket is in the
-- inbox until an agent attaches it to a ticket or discards it.
CREATE TABLE messages (
    id              uuid PRIMARY KEY,
    ticket_id       uuid REFERENCES tickets (id),
    direction       text NOT NULL CHECK (direction IN ('inbound', 'outbound')),
    -- The Message-ID header, used to thread replies and to skip duplicates.
    rfc_message_id  text UNIQUE,
    in_reply_to     text,
    "references"    text[] NOT NULL DEFAULT '{}',
    from_address    text NOT NULL,
    from_name       text,
    subject         text NOT NULL DEFAULT '',
    body_text       text NOT NULL DEFAULT '',
    raw             bytea,
    received_at     timestamptz NOT NULL DEFAULT now(),
    discarded_at    timestamptz,
    -- Only received messages can wait in the inbox, and a discarded one is never on a ticket.
    CHECK (ticket_id IS NOT NULL OR direction = 'inbound'),
    CHECK (discarded_at IS NULL OR ticket_id IS NULL)
);

CREATE INDEX messages_ticket ON messages (ticket_id, received_at);
CREATE INDEX messages_inbox ON messages (received_at) WHERE ticket_id IS NULL AND discarded_at IS NULL;

-- How far the core has read each IMAP mailbox.
CREATE TABLE mailbox_cursors (
    mailbox      text PRIMARY KEY,
    uidvalidity  bigint NOT NULL,
    last_uid     bigint NOT NULL
);
