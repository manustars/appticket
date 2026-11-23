-- A requester's details (docs/domain-rules.md, "People"): phone numbers, a job title, internal
-- notes and a site of their own customer that is not retired.

-- Every entry trimmed and not blank, line ends and tabs included.
CREATE FUNCTION all_trimmed(entries text[]) RETURNS boolean LANGUAGE sql IMMUTABLE AS $$
    SELECT coalesce(bool_and(entry ~ '^\S(.*\S)?$'), true) FROM unnest(entries) AS entry
$$;

-- Names are kept trimmed, and a blank one is no name; those already there follow.
UPDATE requesters SET name = nullif(regexp_replace(name, '^\s+|\s+$', '', 'g'), '')
    WHERE name IS NOT NULL;

ALTER TABLE requesters
    ADD CONSTRAINT requesters_name_trimmed CHECK (name ~ '^\S(.*\S)?$'),
    ADD COLUMN phone_numbers text[] NOT NULL DEFAULT '{}' CHECK (all_trimmed(phone_numbers)),
    ADD COLUMN job_title text CHECK (job_title ~ '^\S(.*\S)?$'),
    ADD COLUMN notes text NOT NULL DEFAULT '',
    ADD COLUMN site_id uuid,
    -- The site is one of the requester's own customer; moving the requester clears it.
    ADD CONSTRAINT requesters_site_of_customer
        FOREIGN KEY (site_id, customer_id) REFERENCES sites (id, customer_id);

CREATE INDEX requesters_site ON requesters (site_id) WHERE site_id IS NOT NULL;

-- Nobody is at a retired site: a requester gets no retired site, and retiring a site needs the
-- requesters at it to have left it first. Checking a requester's site locks it, so that a site
-- retired at the same time is seen.
CREATE FUNCTION nobody_at_a_retired_site() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'requesters' THEN
        IF NEW.site_id IS NOT NULL
           AND EXISTS (SELECT 1 FROM sites WHERE id = NEW.site_id AND retired_at IS NOT NULL
                       FOR SHARE) THEN
            RAISE EXCEPTION 'a requester cannot be at a retired site'
                USING ERRCODE = 'check_violation';
        END IF;
    ELSIF NEW.retired_at IS NOT NULL
          AND EXISTS (SELECT 1 FROM requesters WHERE site_id = NEW.id) THEN
        RAISE EXCEPTION 'a retired site has nobody at it'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER requesters_at_active_sites
    AFTER INSERT OR UPDATE OF site_id ON requesters
    FOR EACH ROW EXECUTE FUNCTION nobody_at_a_retired_site();

CREATE TRIGGER retired_sites_are_empty
    AFTER UPDATE OF retired_at ON sites
    FOR EACH ROW EXECUTE FUNCTION nobody_at_a_retired_site();
