-- The reference an add-on gives a ticket it opens (docs/domain-rules.md, "Add-ons"), such as the
-- id of the monitoring problem the ticket is about: it names that ticket for good, so that opening
-- again with it opens no second ticket, and an add-on opens at most one ticket per problem.

ALTER TABLE tickets
    ADD COLUMN addon_reference text,
    -- Only on a ticket an add-on opened, whose reference it is.
    ADD CONSTRAINT tickets_addon_reference_has_its_addon
        CHECK (addon_reference IS NULL OR opened_by_addon_id IS NOT NULL),
    -- Trimmed, not blank, without control characters and no longer than the add-on API states.
    ADD CONSTRAINT tickets_addon_reference_fits
        CHECK (addon_reference ~ '^\S(.*\S)?$' AND addon_reference !~ '[[:cntrl:]]'
               AND char_length(addon_reference) <= 255);

-- A reference names one ticket of the add-on that gave it; another add-on's references are its
-- own. Tickets without one are not compared.
CREATE UNIQUE INDEX tickets_addon_reference ON tickets (opened_by_addon_id, addon_reference);

-- The reference is given when the add-on opens the ticket and stays: nothing adds one later,
-- changes it, clears it or gives it to another add-on.
CREATE FUNCTION tickets_keep_their_addon_reference() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.addon_reference IS DISTINCT FROM OLD.addon_reference
       OR (OLD.addon_reference IS NOT NULL
           AND NEW.opened_by_addon_id IS DISTINCT FROM OLD.opened_by_addon_id) THEN
        RAISE EXCEPTION 'a ticket keeps the reference the add-on that opened it gave it'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER tickets_keep_their_addon_reference
    AFTER UPDATE OF addon_reference, opened_by_addon_id ON tickets
    FOR EACH ROW EXECUTE FUNCTION tickets_keep_their_addon_reference();
