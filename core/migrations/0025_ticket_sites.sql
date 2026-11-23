-- The sites a ticket is at (docs/domain-rules.md, "Customers, assets and services"): a ticket that
-- is not closed is at the sites of the assets it links, or, when it links none, at its requester's
-- site; a closed ticket stays at the sites it was at when it was closed.

-- The sites a ticket is at by the rule for tickets that are not closed: where each asset it links
-- is (asset_sites), or, when it links none, its requester's site.
CREATE VIEW ticket_sites_by_rule AS
    SELECT ta.ticket_id, w.site_id
    FROM ticket_assets ta
    JOIN asset_sites w ON w.asset_id = ta.asset_id
    WHERE w.site_id IS NOT NULL
    UNION
    SELECT t.id, r.site_id
    FROM tickets t
    JOIN requesters r ON r.id = t.requester_id
    WHERE r.site_id IS NOT NULL
      AND NOT EXISTS (SELECT 1 FROM ticket_assets ta WHERE ta.ticket_id = t.id);

-- The sites each closed ticket was at when it was closed.
CREATE TABLE closed_ticket_sites (
    ticket_id  uuid NOT NULL REFERENCES tickets (id),
    site_id    uuid NOT NULL REFERENCES sites (id),
    PRIMARY KEY (ticket_id, site_id)
);

-- The tickets closed before now stay at the sites they are at by the rule today, the nearest to
-- where they were when they were closed that is known.
INSERT INTO closed_ticket_sites (ticket_id, site_id)
    SELECT r.ticket_id, r.site_id
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
        INSERT INTO closed_ticket_sites (ticket_id, site_id)
            SELECT ticket_id, site_id FROM ticket_sites_by_rule WHERE ticket_id = NEW.id;
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
