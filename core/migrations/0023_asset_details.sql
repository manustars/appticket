-- An asset's details (docs/domain-rules.md, "Customers, assets and services"): optionally a serial
-- number, a site of its customer that is not retired and a holder, who is a requester of its
-- customer and not departed. An asset whose kind moves with its holder is at its holder's site
-- while it has a holder, and at its own site otherwise; any other asset is at its own site.

ALTER TABLE assets
    -- Trimmed and not blank: no white space, line ends included, at either end.
    ADD COLUMN serial_number text CHECK (serial_number ~ '^\S(.*\S)?$'),
    ADD COLUMN site_id uuid,
    ADD COLUMN holder_id uuid,
    -- The site is one of the asset's own customer.
    ADD CONSTRAINT assets_site_of_customer
        FOREIGN KEY (site_id, customer_id) REFERENCES sites (id, customer_id),
    -- The holder is a requester of the asset's own customer: a requester's customer does not
    -- change, nor is the requester merged away, while they hold an asset.
    ADD CONSTRAINT assets_holder_of_customer
        FOREIGN KEY (holder_id, customer_id) REFERENCES requesters (id, customer_id),
    -- A retired asset loses its holder.
    ADD CONSTRAINT retired_assets_have_no_holder CHECK (retired_at IS NULL OR holder_id IS NULL);

CREATE INDEX assets_site ON assets (site_id) WHERE site_id IS NOT NULL;
CREATE INDEX assets_holder ON assets (holder_id) WHERE holder_id IS NOT NULL;

-- The site each asset is at, following its kind; none when it is at no site.
CREATE VIEW asset_sites AS
    SELECT a.id AS asset_id,
           CASE WHEN k.moves_with_holder AND a.holder_id IS NOT NULL THEN h.site_id
                ELSE a.site_id END AS site_id
    FROM assets a
    JOIN asset_kinds k ON k.id = a.kind_id
    LEFT JOIN requesters h ON h.id = a.holder_id;
