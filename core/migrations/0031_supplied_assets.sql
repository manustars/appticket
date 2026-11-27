-- The assets an add-on supplies (docs/domain-rules.md, "Add-ons"): an asset's source is a
-- registered add-on, which knows the asset by a reference of its own, and stays its source. Only
-- that add-on changes the kind, name, serial number and site it supplied, and retires the asset;
-- the core checks who is asking, which the database cannot know.

ALTER TABLE assets
    ADD CONSTRAINT assets_source_is_an_addon
        FOREIGN KEY (source_addon) REFERENCES addons (name),
    -- Trimmed and not blank: no white space, line ends included, at either end.
    ADD CONSTRAINT assets_source_ref_trimmed CHECK (source_ref ~ '^\S(.*\S)?$');

-- An asset has at most one source, for good: another add-on does not take it over, nor does it
-- lose its source or change its reference. An asset entered by hand may get one.
CREATE FUNCTION assets_keep_their_source() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.source_addon IS NOT NULL
       AND (NEW.source_addon IS DISTINCT FROM OLD.source_addon
            OR NEW.source_ref IS DISTINCT FROM OLD.source_ref) THEN
        RAISE EXCEPTION 'an asset keeps its source and the reference its source knows it by'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER assets_keep_their_source
    AFTER UPDATE OF source_addon, source_ref ON assets
    FOR EACH ROW EXECUTE FUNCTION assets_keep_their_source();
