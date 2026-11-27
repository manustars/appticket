-- The conflicts an add-on reports (docs/domain-rules.md, "Add-ons"): what of its system it does
-- not import because no external reference covers it or two do, each with a sentence those who
-- manage add-ons can act on. The add-on sends the whole list and the core keeps the latest one,
-- to show in the add-on's state, also while the add-on is switched off.

CREATE TABLE conflict_lists (
    addon       text PRIMARY KEY REFERENCES addons (name),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- A sentence per language, as a rationale is written: one to four language tags of 2 to 8
-- lowercase letters, each with a text that is trimmed, not blank, without control characters and
-- at most 500 characters long.
CREATE FUNCTION is_conflict_sentence(sentence jsonb) RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT AS $$
    SELECT CASE WHEN jsonb_typeof(sentence) = 'object' THEN
        (SELECT count(*) FROM jsonb_object_keys(sentence)) BETWEEN 1 AND 4
        AND NOT EXISTS (
            SELECT 1 FROM jsonb_each(sentence) AS s (language, text)
            WHERE s.language !~ '^[a-z]{2,8}$'
               OR jsonb_typeof(s.text) <> 'string'
               OR NOT (s.text #>> '{}' ~ '^\S(.*\S)?$' AND s.text #>> '{}' !~ '[[:cntrl:]]'
                       AND char_length(s.text #>> '{}') <= 500))
    ELSE false END
$$;

CREATE TABLE reported_conflicts (
    addon     text NOT NULL REFERENCES conflict_lists (addon) ON DELETE CASCADE,
    -- Where it is in the list, as the add-on sent it: a list holds at most 1000 conflicts.
    position  integer NOT NULL CHECK (position BETWEEN 0 AND 999),
    sentence  jsonb NOT NULL CHECK (is_conflict_sentence(sentence)),
    PRIMARY KEY (addon, position)
);
