-- Agents' work on tickets: replies (outbound messages, sent from an outbox) and notes.

-- When the help desk received the message, or when an agent wrote it.
ALTER TABLE messages RENAME COLUMN received_at TO created_at;

ALTER TABLE messages
    ADD COLUMN author_id      uuid REFERENCES agents (id),
    ADD COLUMN to_address     text,
    -- Outbox: an outbound message is sent when sent_at is set; failures are retried.
    ADD COLUMN sent_at        timestamptz,
    ADD COLUMN send_attempts  integer NOT NULL DEFAULT 0,
    ADD COLUMN last_error     text,
    ADD CONSTRAINT messages_outbound_has_author_and_recipient
        CHECK (direction = 'inbound' OR (author_id IS NOT NULL AND to_address IS NOT NULL)),
    ADD CONSTRAINT messages_only_outbound_is_sent
        CHECK (direction = 'outbound' OR (sent_at IS NULL AND send_attempts = 0));

CREATE INDEX messages_outbox ON messages (created_at)
    WHERE direction = 'outbound' AND sent_at IS NULL;

-- Internal remarks on a ticket, seen only by agents and never sent.
CREATE TABLE notes (
    id          uuid PRIMARY KEY,
    ticket_id   uuid NOT NULL REFERENCES tickets (id),
    author_id   uuid NOT NULL REFERENCES agents (id),
    body        text NOT NULL CHECK (btrim(body) <> ''),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX notes_ticket ON notes (ticket_id, created_at);
