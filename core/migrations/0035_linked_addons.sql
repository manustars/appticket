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

-- An add-on is not linked to an asset it is the source of, and a reference names one asset of an
-- add-on, whether the add-on is its source or linked to it. Raised with a constraint's name, so
-- that the core can tell this refusal apart.
CREATE FUNCTION asset_linked_addons_fit() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM assets WHERE id = NEW.asset_id AND source_addon = NEW.addon) THEN
        RAISE EXCEPTION 'an add-on is not linked to an asset it is the source of'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'asset_linked_addons_not_the_source';
    END IF;
    IF EXISTS (SELECT 1 FROM assets WHERE source_addon = NEW.addon AND source_ref = NEW.reference)
    THEN
        RAISE EXCEPTION 'a reference of an add-on names one asset'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'addon_reference_names_one_asset';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER asset_linked_addons_fit
    AFTER INSERT ON asset_linked_addons
    FOR EACH ROW EXECUTE FUNCTION asset_linked_addons_fit();

-- The same rules seen from the asset: its source is not linked to it, and the reference its source
-- knows it by names no asset the source is linked to.
CREATE FUNCTION assets_source_not_linked() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.source_addon IS NOT NULL AND EXISTS (
        SELECT 1 FROM asset_linked_addons
        WHERE addon = NEW.source_addon AND asset_id = NEW.id) THEN
        RAISE EXCEPTION 'an add-on is not linked to an asset it is the source of'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'asset_linked_addons_not_the_source';
    END IF;
    IF NEW.source_addon IS NOT NULL AND EXISTS (
        SELECT 1 FROM asset_linked_addons
        WHERE addon = NEW.source_addon AND reference = NEW.source_ref) THEN
        RAISE EXCEPTION 'a reference of an add-on names one asset'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'addon_reference_names_one_asset';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER assets_source_not_linked
    AFTER INSERT OR UPDATE OF source_addon, source_ref ON assets
    FOR EACH ROW EXECUTE FUNCTION assets_source_not_linked();
