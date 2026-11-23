-- Announcements from a parent ticket (docs/domain-rules.md, "Tickets"): at any time while the
-- parent is not closed, an agent who can reply to requesters sends one, as often as needed. It
-- goes to the key requesters of the parent's children's customers and of the customers that use the
-- services the parent links, each person once, and stays on the parent with when it was sent and to
-- whom.

CREATE TABLE announcements (
    id          uuid PRIMARY KEY,
    -- The parent ticket it was sent from.
    ticket_id   uuid NOT NULL REFERENCES tickets (id),
    author_id   uuid NOT NULL REFERENCES agents (id),
    subject     text NOT NULL,
    body        text NOT NULL CHECK (btrim(body) <> ''),
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX announcements_ticket ON announcements (ticket_id, created_at);

-- An announcement is sent from a parent ticket that is not closed: a ticket of the help desk's own
-- organization that is not a child. The ticket is locked, so that one being closed or attached at
-- the same time is seen.
CREATE FUNCTION announcements_come_from_parents_not_closed() RETURNS trigger
    LANGUAGE plpgsql AS $$
DECLARE
    ticket record;
BEGIN
    SELECT t.state, t.parent_id, c.is_help_desk INTO ticket
    FROM tickets t JOIN customers c ON c.id = t.customer_id
    WHERE t.id = NEW.ticket_id
    FOR SHARE OF t;
    IF NOT ticket.is_help_desk OR ticket.parent_id IS NOT NULL THEN
        RAISE EXCEPTION 'an announcement is sent from a parent ticket, of the help desk''s own organization'
            USING ERRCODE = 'check_violation';
    END IF;
    IF ticket.state = 'closed' THEN
        RAISE EXCEPTION 'a closed ticket sends no announcements'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER announcements_come_from_parents_not_closed
    AFTER INSERT OR UPDATE OF ticket_id ON announcements
    FOR EACH ROW EXECUTE FUNCTION announcements_come_from_parents_not_closed();

-- Who an announcement went to, as they were when it was sent: the list does not change when a
-- recipient later changes their contact address, stops being a key requester, moves or is merged
-- away (which removes the requester, so there is no key on them).
CREATE TABLE announcement_recipients (
    announcement_id  uuid NOT NULL REFERENCES announcements (id),
    requester_id     uuid NOT NULL,
    customer_id      uuid NOT NULL REFERENCES customers (id),
    name             text,
    -- Their contact address when it was sent.
    address          text NOT NULL,
    -- Each person once.
    PRIMARY KEY (announcement_id, requester_id),
    -- Target of the key that sends each message of an announcement to one of its recipients.
    UNIQUE (announcement_id, address)
);

-- An announcement goes to someone. The check runs at commit, so that an announcement and its
-- recipients can be added in one transaction.
CREATE FUNCTION announcements_have_recipients() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM announcement_recipients WHERE announcement_id = NEW.id) THEN
        RAISE EXCEPTION 'an announcement goes to someone: a key requester of a customer the failure affects'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER announcements_have_recipients
    AFTER INSERT ON announcements
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION announcements_have_recipients();

-- Each recipient is, when the announcement is sent, a key requester at their contact address, of a
-- customer of one of the parent's children or that uses a service the parent links. The key
-- requester is locked, so that one being unmarked at the same time is seen.
CREATE FUNCTION announcements_go_to_key_requesters_affected() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM announcements n
        JOIN key_requesters k ON k.requester_id = NEW.requester_id
        WHERE n.id = NEW.announcement_id
          AND k.customer_id = NEW.customer_id AND k.contact_address = NEW.address
          AND (EXISTS (SELECT 1 FROM tickets t
                       WHERE t.parent_id = n.ticket_id AND t.customer_id = k.customer_id)
               OR EXISTS (SELECT 1 FROM ticket_services ts
                          JOIN customer_services cs ON cs.service_id = ts.service_id
                          WHERE ts.ticket_id = n.ticket_id AND cs.customer_id = k.customer_id))
        FOR SHARE OF k) THEN
        RAISE EXCEPTION 'an announcement goes to a key requester, at their contact address, of a customer of the parent''s children or of its services'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER announcements_go_to_key_requesters_affected
    AFTER INSERT OR UPDATE ON announcement_recipients
    FOR EACH ROW EXECUTE FUNCTION announcements_go_to_key_requesters_affected();

-- The message an announcement is sent in, one per recipient: it leaves through the outbox like a
-- reply, and a reply to it comes back to the parent.
ALTER TABLE messages
    ADD COLUMN announcement_id uuid,
    ADD CONSTRAINT messages_to_an_announcement_recipient
        FOREIGN KEY (announcement_id, to_address)
        REFERENCES announcement_recipients (announcement_id, address),
    ADD CONSTRAINT messages_announcements_are_outbound
        CHECK (announcement_id IS NULL OR direction = 'outbound');

CREATE UNIQUE INDEX messages_one_per_announcement_recipient
    ON messages (announcement_id, to_address) WHERE announcement_id IS NOT NULL;

-- Each message the help desk sends on a ticket has one recipient, never shared between people: the
-- ticket's requester, at one of their addresses, or a key requester of the ticket's customer, at
-- their contact address; or, for an announcement sent from the ticket, one of its recipients.
CREATE OR REPLACE FUNCTION messages_go_to_the_requester_or_key_requesters() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.announcement_id IS NOT NULL THEN
        IF NOT EXISTS (SELECT 1 FROM announcements
                       WHERE id = NEW.announcement_id AND ticket_id = NEW.ticket_id) THEN
            RAISE EXCEPTION 'an announcement''s messages are on the ticket it was sent from'
                USING ERRCODE = 'check_violation';
        END IF;
        RETURN NULL;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM tickets t
                   JOIN requester_addresses a ON a.requester_id = t.requester_id
                   WHERE t.id = NEW.ticket_id AND a.address = NEW.to_address)
       AND NOT EXISTS (SELECT 1 FROM tickets t
                       JOIN key_requesters k ON k.customer_id = t.customer_id
                       WHERE t.id = NEW.ticket_id AND k.contact_address = NEW.to_address) THEN
        RAISE EXCEPTION 'a message goes to the ticket''s requester or to a key requester of its customer'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;
