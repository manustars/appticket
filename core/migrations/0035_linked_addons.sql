-- Linked add-ons (docs/domain-rules.md, "Add-ons"; ADR 0002): besides its source, an asset can be
-- linked to other add-ons, each knowing it by a reference of its own, when agents who manage the
-- catalog accept a link proposal. A linked add-on changes nothing of the asset and only supplies
-- live details and links; it unlinks itself when the asset leaves its system, which retires
-- nothing.

ALTER TABLE asset_link_proposals
    DROP CONSTRAINT asset_link_proposals_state_check,
    ADD CONSTRAINT asset_link_proposals_state_check
        CHECK (state IN ('open', 'accepted', 'discarded')),
    -- What a link made by accepting a proposal refers to.
    ADD CONSTRAINT asset_link_proposals_link_key UNIQUE (id, addon, reference, asset_id);

CREATE TABLE asset_linked_addons (
    asset_id     uuid NOT NULL REFERENCES assets (id),
    addon        text NOT NULL REFERENCES addons (name),
    reference    text NOT NULL CHECK (reference ~ '^\S(.*\S)?$'),
    -- The proposal agents accepted, which proposed this add-on, reference and asset.
    proposal_id  uuid NOT NULL UNIQUE,
    created_at   timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (proposal_id, addon, reference, asset_id)
        REFERENCES asset_link_proposals (id, addon, reference, asset_id),
    -- A reference of an add-on names one asset, and an add-on is linked to an asset once.
    PRIMARY KEY (addon, reference),
    UNIQUE (asset_id, addon)
);

-- A link is made and removed, never changed.
CREATE FUNCTION asset_linked_addons_stay() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'a linked add-on is unlinked, not changed'
        USING ERRCODE = 'check_violation';
END;
$$;

CREATE TRIGGER asset_linked_addons_stay
    BEFORE UPDATE ON asset_linked_addons
    FOR EACH ROW EXECUTE FUNCTION asset_linked_addons_stay();

-- An accepted proposal stays accepted, so that the links it made keep a reason.
CREATE FUNCTION asset_link_proposals_accepted_stay() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state = 'accepted' AND NEW.state <> OLD.state THEN
        RAISE EXCEPTION 'an accepted link proposal stays accepted'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER asset_link_proposals_accepted_stay
    BEFORE UPDATE OF state ON asset_link_proposals
    FOR EACH ROW EXECUTE FUNCTION asset_link_proposals_accepted_stay();

-- An add-on is not linked to an asset it is the source of, and a reference names one asset of an
-- add-on, whether the add-on is its source or linked to it; checked after a link is made or an
-- asset gets a source, refused with a constraint's name so that the core can tell the refusals
-- apart.
--
-- The rows each check looks for may be written by a transaction not yet committed, which no row
-- lock reaches: so the check first takes advisory locks on the add-on's reference and on the
-- add-on at the asset, always in this order, and a transaction linking and one giving a source at
-- the same time wait for each other; the later then sees what the earlier committed, since each
-- statement here reads afresh.
CREATE FUNCTION addon_references_fit(p_addon text, p_asset uuid, p_reference text)
    RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_advisory_xact_lock(
        hashtextextended('addon reference ' || p_addon || ' ' || p_reference, 0));
    PERFORM pg_advisory_xact_lock(
        hashtextextended('addon asset ' || p_addon || ' ' || p_asset::text, 0));
    IF EXISTS (SELECT 1 FROM assets a
               JOIN asset_linked_addons l ON l.asset_id = a.id AND l.addon = a.source_addon
               WHERE a.id = p_asset AND a.source_addon = p_addon) THEN
        RAISE EXCEPTION 'an add-on is not linked to an asset it is the source of'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'asset_linked_addons_not_the_source';
    END IF;
    IF EXISTS (SELECT 1 FROM assets a
               JOIN asset_linked_addons l ON l.addon = a.source_addon AND l.reference = a.source_ref
               WHERE a.source_addon = p_addon AND a.source_ref = p_reference) THEN
        RAISE EXCEPTION 'a reference of an add-on names one asset'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'addon_reference_names_one_asset';
    END IF;
END;
$$;

-- A link is made only as an accepted proposal proposed it (the foreign key says what it proposed;
-- its state is checked here, the proposal locked against being decided meanwhile), and fits the
-- add-on's other references.
CREATE FUNCTION asset_linked_addons_fit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    PERFORM 1 FROM asset_link_proposals WHERE id = NEW.proposal_id AND state = 'accepted'
        FOR SHARE;
    IF NOT FOUND THEN
        RAISE EXCEPTION 'a link is made by accepting the link proposal that proposed it'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'asset_linked_addons_accepted';
    END IF;
    PERFORM addon_references_fit(NEW.addon, NEW.asset_id, NEW.reference);
    RETURN NULL;
END;
$$;

CREATE TRIGGER asset_linked_addons_fit
    AFTER INSERT ON asset_linked_addons
    FOR EACH ROW EXECUTE FUNCTION asset_linked_addons_fit();

-- The same rules seen from an asset given a source.
CREATE FUNCTION assets_source_not_linked() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.source_addon IS NOT NULL THEN
        PERFORM addon_references_fit(NEW.source_addon, NEW.id, NEW.source_ref);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER assets_source_not_linked
    AFTER INSERT OR UPDATE OF source_addon, source_ref ON assets
    FOR EACH ROW EXECUTE FUNCTION assets_source_not_linked();

