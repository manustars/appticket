-- What an agent is writing on a ticket and has not sent yet, kept by the core so that it survives
-- leaving the page or changing computer. One draft per agent and ticket; sending clears it.
CREATE TABLE drafts (
    ticket_id   uuid NOT NULL REFERENCES tickets (id),
    agent_id    uuid NOT NULL REFERENCES agents (id),
    mode        text NOT NULL CHECK (mode IN ('reply', 'note')),
    body        text NOT NULL,
    updated_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (ticket_id, agent_id)
);

-- Add-ons may also propose the services a ticket is about.
ALTER TABLE proposals DROP CONSTRAINT proposals_kind_check;
ALTER TABLE proposals ADD CONSTRAINT proposals_kind_check
    CHECK (kind IN ('triage', 'priority', 'assets', 'services', 'reply'));
