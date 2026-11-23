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

-- No asset is at a retired site or held by a departed requester: an asset gets neither, and
-- retiring a site or departing needs the assets at it, or held, to have left them first. Checking an
-- asset's site and holder locks their rows whatever they hold, so that a site being retired or a
-- holder departing at the same time is waited for and seen.
CREATE FUNCTION assets_at_active_sites_with_present_holders() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.site_id IS NOT NULL
       AND (TG_OP = 'INSERT' OR NEW.site_id IS DISTINCT FROM OLD.site_id)
       AND (SELECT retired_at IS NOT NULL FROM sites WHERE id = NEW.site_id FOR SHARE) THEN
        RAISE EXCEPTION 'an asset cannot be at a retired site'
            USING ERRCODE = 'check_violation';
    END IF;
    IF NEW.holder_id IS NOT NULL
       AND (TG_OP = 'INSERT' OR NEW.holder_id IS DISTINCT FROM OLD.holder_id)
       AND (SELECT departed_at IS NOT NULL FROM requesters WHERE id = NEW.holder_id FOR SHARE) THEN
        RAISE EXCEPTION 'a departed requester holds no asset'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER assets_at_active_sites_with_present_holders
    AFTER INSERT OR UPDATE OF site_id, holder_id ON assets
    FOR EACH ROW EXECUTE FUNCTION assets_at_active_sites_with_present_holders();

CREATE FUNCTION retired_sites_have_no_assets() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM assets WHERE site_id = NEW.id) THEN
        RAISE EXCEPTION 'a retired site has no asset at it'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER retired_sites_have_no_assets
    AFTER UPDATE OF retired_at ON sites
    FOR EACH ROW
    WHEN (NEW.retired_at IS NOT NULL)
    EXECUTE FUNCTION retired_sites_have_no_assets();

CREATE FUNCTION departed_requesters_hold_no_assets() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM assets WHERE holder_id = NEW.id) THEN
        RAISE EXCEPTION 'a holder does not depart: the assets they hold lose their holder first'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER departed_requesters_hold_no_assets
    AFTER UPDATE OF departed_at ON requesters
    FOR EACH ROW
    WHEN (NEW.departed_at IS NOT NULL)
    EXECUTE FUNCTION departed_requesters_hold_no_assets();
