-- What tickets refer to (ADR 0002, ADR 0003): assets a customer owns, and services the help desk
-- provides to the customers that use them. The rules in docs/domain-rules.md are enforced by the
-- composite keys below.

-- Target of the keys that keep a ticket's links inside the ticket's customer.
ALTER TABLE tickets ADD CONSTRAINT tickets_id_customer UNIQUE (id, customer_id);

CREATE TABLE assets (
    id            uuid PRIMARY KEY,
    customer_id   uuid NOT NULL REFERENCES customers (id),
    kind          text NOT NULL CHECK (btrim(kind) <> ''),
    name          text NOT NULL CHECK (btrim(name) <> ''),
    -- The add-on an asset came from and that add-on's identifier for it, when it has a source.
    source_addon  text,
    source_ref    text,
    created_at    timestamptz NOT NULL DEFAULT now(),
    -- A retired asset stays on the tickets that refer to it but gets no new links.
    retired_at    timestamptz,
    CHECK ((source_addon IS NULL) = (source_ref IS NULL)),
    UNIQUE (source_addon, source_ref),
    UNIQUE (id, customer_id)
);

CREATE INDEX assets_customer ON assets (customer_id);

CREATE TABLE services (
    id           uuid PRIMARY KEY,
    name         text NOT NULL UNIQUE CHECK (btrim(name) <> ''),
    description  text NOT NULL DEFAULT '',
    created_at   timestamptz NOT NULL DEFAULT now(),
    retired_at   timestamptz
);

-- Which customers use which services. The help desk's own organization uses every service.
CREATE TABLE customer_services (
    customer_id  uuid NOT NULL REFERENCES customers (id),
    service_id   uuid NOT NULL REFERENCES services (id),
    PRIMARY KEY (customer_id, service_id)
);

-- The assets a service runs on; they can belong to any customer.
CREATE TABLE service_assets (
    service_id  uuid NOT NULL REFERENCES services (id),
    asset_id    uuid NOT NULL REFERENCES assets (id),
    PRIMARY KEY (service_id, asset_id)
);

-- A ticket links only assets of its own customer...
CREATE TABLE ticket_assets (
    ticket_id    uuid NOT NULL,
    customer_id  uuid NOT NULL,
    asset_id     uuid NOT NULL,
    PRIMARY KEY (ticket_id, asset_id),
    FOREIGN KEY (ticket_id, customer_id) REFERENCES tickets (id, customer_id) ON UPDATE CASCADE,
    FOREIGN KEY (asset_id, customer_id) REFERENCES assets (id, customer_id)
);

-- ...and only services its customer uses.
CREATE TABLE ticket_services (
    ticket_id    uuid NOT NULL,
    customer_id  uuid NOT NULL,
    service_id   uuid NOT NULL,
    PRIMARY KEY (ticket_id, service_id),
    FOREIGN KEY (ticket_id, customer_id) REFERENCES tickets (id, customer_id) ON UPDATE CASCADE,
    FOREIGN KEY (customer_id, service_id) REFERENCES customer_services (customer_id, service_id)
);
