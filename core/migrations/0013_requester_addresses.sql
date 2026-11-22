-- A requester has one or more e-mail addresses, and each address belongs to exactly one requester
-- (docs/domain-rules.md, "People").

CREATE TABLE requester_addresses (
    address       text PRIMARY KEY CHECK (address = lower(address) AND address LIKE '%_@_%'),
    requester_id  uuid NOT NULL REFERENCES requesters (id),
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX requester_addresses_requester ON requester_addresses (requester_id, created_at);

-- The requesters there are already keep their address, registered when they were.
INSERT INTO requester_addresses (address, requester_id, created_at)
    SELECT email, id, created_at FROM requesters;

ALTER TABLE requesters DROP COLUMN email;

-- The address the ticket's requester last wrote from on the ticket, while it is still theirs: a
-- reply goes there. Null when they have not written on it (or the ticket has no requester), and the
-- agent picks one of their addresses. Others can write on a ticket too, since replies are
-- recognised by their headers; their messages do not count.
CREATE FUNCTION ticket_reply_address(ticket uuid) RETURNS text LANGUAGE sql STABLE AS $$
    SELECT m.from_address
    FROM tickets t
    JOIN messages m ON m.ticket_id = t.id AND m.direction = 'inbound'
    JOIN requester_addresses a ON a.address = m.from_address AND a.requester_id = t.requester_id
    WHERE t.id = ticket
    ORDER BY m.created_at DESC, m.id
    LIMIT 1
$$;

-- The address a requester registered first, which stands for them in lists.
CREATE FUNCTION requester_first_address(requester uuid) RETURNS text LANGUAGE sql STABLE AS $$
    SELECT address FROM requester_addresses WHERE requester_id = requester
    ORDER BY created_at, address
    LIMIT 1
$$;

-- A requester always has at least one address. The check runs at commit, so that a requester and
-- its first address can be added in either order within one transaction.
CREATE FUNCTION requester_has_an_address() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    requester uuid;
BEGIN
    IF TG_TABLE_NAME = 'requesters' THEN
        requester := NEW.id;
    ELSE
        requester := OLD.requester_id;
    END IF;
    IF EXISTS (SELECT 1 FROM requesters WHERE id = requester)
       AND NOT EXISTS (SELECT 1 FROM requester_addresses WHERE requester_id = requester) THEN
        RAISE EXCEPTION 'a requester needs at least one e-mail address'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER requesters_have_an_address
    AFTER INSERT ON requesters
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION requester_has_an_address();

CREATE CONSTRAINT TRIGGER addresses_leave_one
    AFTER DELETE OR UPDATE OF requester_id ON requester_addresses
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION requester_has_an_address();
