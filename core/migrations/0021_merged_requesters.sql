-- Merging requesters (docs/domain-rules.md, "People"): two requesters of the same customer who are
-- one person are merged, and the addresses and tickets of one pass to the other, which remains. A
-- merge cannot be undone, and requesters of different customers are never merged.

-- Every merge: the requester merged away, who no longer exists, and the one that remained. When
-- the one that remained is merged in turn, the requester it was merged into takes its place here.
CREATE TABLE requester_merges (
    merged_id  uuid PRIMARY KEY,
    kept_id    uuid NOT NULL REFERENCES requesters (id),
    merged_at  timestamptz NOT NULL DEFAULT now(),
    CHECK (merged_id <> kept_id)
);

CREATE INDEX requester_merges_kept ON requester_merges (kept_id);

-- The two requesters of a merge belong to the same customer, which is not offboarded (its
-- requesters do not change), and neither has departed. Both are locked, so that one moving,
-- departing or being merged at the same time is seen.
CREATE FUNCTION merges_join_requesters_of_one_customer() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM requesters m, requesters k, customers c
                   WHERE m.id = NEW.merged_id AND k.id = NEW.kept_id
                     AND m.customer_id = k.customer_id AND c.id = k.customer_id
                     AND m.departed_at IS NULL AND k.departed_at IS NULL
                     AND c.offboarded_at IS NULL
                   FOR UPDATE OF m, k) THEN
        RAISE EXCEPTION 'only two present requesters of the same customer, not offboarded, are merged'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER merges_of_one_customer
    AFTER INSERT ON requester_merges
    FOR EACH ROW EXECUTE FUNCTION merges_join_requesters_of_one_customer();

-- A merge removes the requester merged away: by commit, their addresses and tickets have passed to
-- the one that remains, and they are gone.
CREATE FUNCTION merged_requesters_are_gone() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM requesters WHERE id = NEW.merged_id) THEN
        RAISE EXCEPTION 'a merged requester does not remain'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

CREATE CONSTRAINT TRIGGER merged_requesters_do_not_remain
    AFTER INSERT ON requester_merges
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION merged_requesters_are_gone();

-- An address leaves its requester only when they departed, for the requester the person became at
-- another customer, or when they are merged, for the requester that remains. The requesters are
-- locked, so that one coming back or being merged at the same time is seen.
CREATE FUNCTION addresses_pass_on_departure_or_merge() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM requester_merges
               WHERE merged_id = OLD.requester_id AND kept_id = NEW.requester_id) THEN
        RETURN NULL;
    END IF;
    IF NOT EXISTS (SELECT 1 FROM requesters r, requesters n
                   WHERE r.id = OLD.requester_id AND n.id = NEW.requester_id
                     AND r.departed_at IS NOT NULL AND r.customer_id <> n.customer_id
                   FOR SHARE) THEN
        RAISE EXCEPTION 'an address passes only from a departed requester to one of another customer, or by a merge'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NULL;
END;
$$;

DROP TRIGGER addresses_pass_from_departed_requesters ON requester_addresses;
DROP FUNCTION addresses_pass_only_from_departed_requesters();

CREATE TRIGGER addresses_pass_on_departure_or_merge
    AFTER UPDATE OF requester_id ON requester_addresses
    FOR EACH ROW
    WHEN (NEW.requester_id IS DISTINCT FROM OLD.requester_id)
    EXECUTE FUNCTION addresses_pass_on_departure_or_merge();
