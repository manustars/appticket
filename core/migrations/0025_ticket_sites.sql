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

CREATE VIEW ticket_sites AS
    SELECT ticket_id, site_id FROM ticket_sites_by_rule;
