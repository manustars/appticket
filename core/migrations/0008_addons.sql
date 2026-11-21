-- Add-ons and the events the core sends them (ADR 0005).

CREATE TABLE addons (
    id            uuid PRIMARY KEY,
    -- A short name, also used as the source of the assets an add-on supplies.
    name          text NOT NULL UNIQUE CHECK (name ~ '^[a-z][a-z0-9-]{1,39}$'),
    -- Where the core sends events: <url>/events.
    url           text NOT NULL,
    -- Signs every event sent to the add-on (HMAC-SHA-256).
    event_secret  text NOT NULL,
    -- The add-on's token for the core's API; only its hash is stored.
    token_hash    bytea NOT NULL UNIQUE,
    -- Event kinds the add-on receives, and what its token may do.
    events        text[] NOT NULL DEFAULT '{}',
    scopes        text[] NOT NULL DEFAULT '{}',
    enabled       boolean NOT NULL DEFAULT true,
    created_at    timestamptz NOT NULL DEFAULT now()
);

-- Something that happened in the core, recorded in the same transaction as the change itself.
CREATE TABLE events (
    id          uuid PRIMARY KEY,
    kind        text NOT NULL,
    data        jsonb NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

-- One row per event and add-on that subscribed to its kind; delivered with retries, and kept
-- waiting while the add-on is switched off.
CREATE TABLE event_deliveries (
    event_id         uuid NOT NULL REFERENCES events (id),
    addon_id         uuid NOT NULL REFERENCES addons (id),
    delivered_at     timestamptz,
    attempts         integer NOT NULL DEFAULT 0,
    last_error       text,
    next_attempt_at  timestamptz,
    PRIMARY KEY (event_id, addon_id)
);

CREATE INDEX event_deliveries_due ON event_deliveries (next_attempt_at)
    WHERE delivered_at IS NULL;
