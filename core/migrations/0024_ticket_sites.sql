-- The sites a ticket is at (docs/domain-rules.md, "Customers, assets and services"): a ticket that
-- is not closed is at the sites of the assets it links, or, when it links none, at its requester's
-- site; a closed ticket stays at the sites it was at when it was closed.

-- The sites a ticket is at by the rule for tickets that are not closed: where each asset it links
-- is (asset_sites), or, when it links none, its requester's site.
CREATE VIEW ticket_sites_by_rule AS
    SELECT ta.ticket_id, located.site_id
    FROM ticket_assets ta
    JOIN asset_sites located ON located.asset_id = ta.asset_id
    WHERE located.site_id IS NOT NULL
    UNION
    SELECT t.id, r.site_id
    FROM tickets t
    JOIN requesters r ON r.id = t.requester_id
    WHERE r.site_id IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM ticket_assets ta WHERE ta.ticket_id = t.id);

-- The sites each closed ticket was at when it was closed: sites of the ticket's own customer.
CREATE TABLE closed_ticket_sites (
    ticket_id    uuid NOT NULL,
    customer_id  uuid NOT NULL,
    site_id      uuid NOT NULL,
    PRIMARY KEY (ticket_id, site_id),
    FOREIGN KEY (ticket_id, customer_id) REFERENCES tickets (id, customer_id),
    FOREIGN KEY (site_id, customer_id) REFERENCES sites (id, customer_id)
);

-- Only a closed ticket keeps sites. Checking the ticket locks it, so that one reopened at the same
-- time, which only a change bypassing the core can do, is seen.
CREATE FUNCTION only_closed_tickets_keep_sites() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF (SELECT state <> 'closed' FROM tickets WHERE id = NEW.ticket_id FOR SHARE) THEN
        RAISE EXCEPTION 'only a closed ticket keeps the sites it was at'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER only_closed_tickets_keep_sites
    AFTER INSERT OR UPDATE ON closed_ticket_sites
    FOR EACH ROW EXECUTE FUNCTION only_closed_tickets_keep_sites();

-- A ticket that moves to another customer, which a closed one does only with its requester (who
-- was identified with the wrong customer), leaves the sites it was kept at: they are its old
-- customer's, and go as its assets do. Deleted under the ticket's lock, before the keys are checked,
-- so that sites kept by a close committed while the move waited for the ticket go too.
CREATE FUNCTION moved_tickets_leave_their_kept_sites() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM closed_ticket_sites WHERE ticket_id = NEW.id AND customer_id <> NEW.customer_id;
    RETURN NEW;
END;
$$;

CREATE TRIGGER moved_tickets_leave_their_kept_sites
    BEFORE UPDATE OF customer_id ON tickets
    FOR EACH ROW
    WHEN (OLD.customer_id IS DISTINCT FROM NEW.customer_id)
    EXECUTE FUNCTION moved_tickets_leave_their_kept_sites();

-- The tickets closed before now stay at the sites they are at by the rule today, the nearest to
-- where they were when they were closed that is known.
INSERT INTO closed_ticket_sites (ticket_id, customer_id, site_id)
    SELECT r.ticket_id, t.customer_id, r.site_id
    FROM ticket_sites_by_rule r
    JOIN tickets t ON t.id = r.ticket_id
    WHERE t.state = 'closed';

-- Closing a ticket keeps the sites it is at by the rule, read in one statement: the close comes
-- either before or after a change to its assets or requester, never halfway through one. A ticket
-- that stops being closed, which only a change bypassing the core can do, follows the rule again.
CREATE FUNCTION closed_tickets_keep_their_sites() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND NEW.state IS NOT DISTINCT FROM OLD.state THEN
        RETURN NULL;
    END IF;
    IF NEW.state = 'closed' THEN
        INSERT INTO closed_ticket_sites (ticket_id, customer_id, site_id)
            SELECT ticket_id, NEW.customer_id, site_id FROM ticket_sites_by_rule
            WHERE ticket_id = NEW.id;
    ELSIF TG_OP = 'UPDATE' AND OLD.state = 'closed' THEN
        DELETE FROM closed_ticket_sites WHERE ticket_id = NEW.id;
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER closed_tickets_keep_their_sites
    AFTER INSERT OR UPDATE OF state ON tickets
    FOR EACH ROW EXECUTE FUNCTION closed_tickets_keep_their_sites();

-- The sites each ticket is at: a closed one where it was when it was closed, any other by the rule.
CREATE VIEW ticket_sites AS
    SELECT s.ticket_id, s.site_id
    FROM closed_ticket_sites s
    JOIN tickets t ON t.id = s.ticket_id AND t.state = 'closed'
    UNION ALL
    SELECT r.ticket_id, r.site_id
    FROM ticket_sites_by_rule r
    JOIN tickets t ON t.id = r.ticket_id AND t.state <> 'closed';
