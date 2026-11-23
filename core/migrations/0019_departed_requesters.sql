-- Departed requesters (docs/domain-rules.md, "People"): a requester who leaves their customer keeps
-- their history, but no ticket is opened for them and their messages wait in the inbox. A departed
-- requester can come back.

ALTER TABLE requesters ADD COLUMN departed_at timestamptz;

-- A requester who has not departed has at least one address. A departed requester can be left with
-- none, once the addresses they kept have passed to the requester they became at another customer,
-- and comes back only with one. The check runs at commit, as before.
CREATE OR REPLACE FUNCTION requester_has_an_address() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    requester uuid;
BEGIN
    IF TG_TABLE_NAME = 'requesters' THEN
        requester := NEW.id;
    ELSE
        requester := OLD.requester_id;
    END IF;
    IF EXISTS (SELECT 1 FROM requesters WHERE id = requester AND departed_at IS NULL)
       AND NOT EXISTS (SELECT 1 FROM requester_addresses WHERE requester_id = requester) THEN
        RAISE EXCEPTION 'a requester needs at least one e-mail address'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

DROP TRIGGER requesters_have_an_address ON requesters;

CREATE CONSTRAINT TRIGGER requesters_have_an_address
    AFTER INSERT OR UPDATE OF departed_at ON requesters
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION requester_has_an_address();

-- No ticket is opened for, or given to, a departed requester; the tickets they had stay theirs.
-- Checking the requester locks it, so that one departing at the same time is seen.
CREATE FUNCTION no_ticket_for_a_departed_requester() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM requesters WHERE id = NEW.requester_id AND departed_at IS NOT NULL
               FOR SHARE) THEN
        RAISE EXCEPTION 'no ticket is opened for a departed requester'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER tickets_of_present_requesters
    AFTER INSERT OR UPDATE OF requester_id ON tickets
    FOR EACH ROW
    WHEN (NEW.requester_id IS NOT NULL)
    EXECUTE FUNCTION no_ticket_for_a_departed_requester();

-- Every message from a departed requester's addresses, replies included, is received into the
-- inbox; an agent can then attach it to a ticket.
CREATE FUNCTION departed_requesters_write_to_the_inbox() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM requester_addresses a JOIN requesters r ON r.id = a.requester_id
               WHERE a.address = NEW.from_address AND r.departed_at IS NOT NULL
               FOR SHARE OF r) THEN
        RAISE EXCEPTION 'a message from a departed requester waits in the inbox'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER messages_of_departed_requesters_wait
    AFTER INSERT ON messages
    FOR EACH ROW
    WHEN (NEW.direction = 'inbound' AND NEW.ticket_id IS NOT NULL)
    EXECUTE FUNCTION departed_requesters_write_to_the_inbox();

-- An address leaves its requester only when they departed, for the requester the person became at
-- another customer. Both requesters are locked, so that one coming back at the same time is seen.
CREATE FUNCTION addresses_pass_only_from_departed_requesters() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM requesters r, requesters n
                   WHERE r.id = OLD.requester_id AND n.id = NEW.requester_id
                     AND r.departed_at IS NOT NULL AND r.customer_id <> n.customer_id
                   FOR SHARE) THEN
        RAISE EXCEPTION 'an address passes only from a departed requester to one of another customer'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER addresses_pass_from_departed_requesters
    AFTER UPDATE OF requester_id ON requester_addresses
    FOR EACH ROW
    WHEN (NEW.requester_id IS DISTINCT FROM OLD.requester_id)
    EXECUTE FUNCTION addresses_pass_only_from_departed_requesters();
