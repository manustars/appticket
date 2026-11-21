-- Ticket priority, and the Assistant's proposals (ADR 0006).

ALTER TABLE tickets
    ADD COLUMN priority text NOT NULL DEFAULT 'normal'
        CHECK (priority IN ('low', 'normal', 'high', 'urgent'));

-- A course of action an add-on suggests on an inbox message (triage) or on a ticket (priority,
-- assets, reply). It takes effect only when an agent accepts it.
CREATE TABLE proposals (
    id          uuid PRIMARY KEY,
    addon_id    uuid NOT NULL REFERENCES addons (id),
    message_id  uuid REFERENCES messages (id),
    ticket_id   uuid REFERENCES tickets (id),
    kind        text NOT NULL CHECK (kind IN ('triage', 'priority', 'assets', 'reply')),
    content     jsonb NOT NULL,
    -- Why the add-on suggests it, shown to the agent.
    rationale   text NOT NULL DEFAULT '',
    state       text NOT NULL DEFAULT 'open'
                CHECK (state IN ('open', 'accepted', 'discarded', 'superseded')),
    created_at  timestamptz NOT NULL DEFAULT now(),
    decided_at  timestamptz,
    decided_by  uuid REFERENCES agents (id),
    -- Triage is about an inbox message; everything else about a ticket.
    CHECK ((message_id IS NULL) <> (ticket_id IS NULL)),
    CHECK ((kind = 'triage') = (message_id IS NOT NULL))
);

-- At most one open proposal of each kind on a message or a ticket.
CREATE UNIQUE INDEX proposals_one_open_on_message ON proposals (message_id, kind)
    WHERE state = 'open' AND message_id IS NOT NULL;
CREATE UNIQUE INDEX proposals_one_open_on_ticket ON proposals (ticket_id, kind)
    WHERE state = 'open' AND ticket_id IS NOT NULL;
