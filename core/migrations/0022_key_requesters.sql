-- Key requesters (docs/domain-rules.md, "People"): requesters who speak for their customer. They
-- get the replies on its tickets that have no requester and the messages about failures that
-- affect it, at their contact address, which is always one of their addresses.

ALTER TABLE requester_addresses
    -- Target of the key that keeps a contact address among its requester's addresses.
    ADD CONSTRAINT requester_addresses_of_requester UNIQUE (address, requester_id);

CREATE TABLE key_requesters (
    requester_id     uuid PRIMARY KEY,
    -- A key requester speaks for their own customer: a requester's customer does not change
    -- while they are one.
    customer_id      uuid NOT NULL,
    -- One of the requester's addresses: it is not removed, nor passed to another requester,
    -- while it is their contact address.
    contact_address  text NOT NULL,
    marked_at        timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (requester_id, customer_id) REFERENCES requesters (id, customer_id),
    FOREIGN KEY (contact_address, requester_id)
        REFERENCES requester_addresses (address, requester_id)
);

CREATE INDEX key_requesters_customer ON key_requesters (customer_id);

-- A key requester has not departed: a departed requester is not marked, and one who departs stops
-- being a key requester first. The requester is locked, so that one departing at the same time is
-- seen.
CREATE FUNCTION key_requesters_are_present() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'key_requesters' THEN
        IF EXISTS (SELECT 1 FROM requesters WHERE id = NEW.requester_id AND departed_at IS NOT NULL
                   FOR SHARE) THEN
            RAISE EXCEPTION 'a departed requester is not a key requester'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF EXISTS (SELECT 1 FROM key_requesters WHERE requester_id = NEW.id) THEN
        RAISE EXCEPTION 'a key requester does not depart: they stop being one first'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER key_requesters_are_present
    AFTER INSERT OR UPDATE OF requester_id ON key_requesters
    FOR EACH ROW EXECUTE FUNCTION key_requesters_are_present();

CREATE TRIGGER departed_requesters_are_not_key
    AFTER UPDATE OF departed_at ON requesters
    FOR EACH ROW
    WHEN (NEW.departed_at IS NOT NULL)
    EXECUTE FUNCTION key_requesters_are_present();

-- Each message the help desk sends on a ticket has one recipient, never shared between people: the
-- ticket's requester, at one of their addresses, or a key requester of the ticket's customer, at
-- their contact address (who replies and resolved children go to: docs/domain-rules.md, "Tickets").
-- A ticket with neither gets no message.
CREATE FUNCTION messages_go_to_the_requester_or_key_requesters() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
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

CREATE TRIGGER messages_go_to_the_requester_or_key_requesters
    AFTER INSERT ON messages
    FOR EACH ROW
    WHEN (NEW.direction = 'outbound')
    EXECUTE FUNCTION messages_go_to_the_requester_or_key_requesters();
