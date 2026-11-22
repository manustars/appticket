-- The asset kinds the help desk manages (docs/domain-rules.md, "Customers, assets and services"):
-- every asset has one, and it says whether its assets move with their holder or stay at their site.

CREATE TABLE asset_kinds (
    id                 uuid PRIMARY KEY,
    -- Trimmed and not blank: no white space, line ends included, at either end.
    name               text NOT NULL CHECK (name ~ '^\S(.*\S)?$'),
    -- Otherwise its assets stay at their own site.
    moves_with_holder  boolean NOT NULL DEFAULT false,
    created_at         timestamptz NOT NULL DEFAULT now()
);

-- Kinds are named, in imports too: no two differ only in case.
CREATE UNIQUE INDEX asset_kinds_name ON asset_kinds (lower(name));

-- Each free-text kind the assets have becomes one entry, staying at its site: spellings that differ
-- only in case or in the white space around them are one kind, spelled as most of its assets have
-- it (the first such spelling in sorting order, on a tie). Kinds were always written trimmed, so none
-- is made of white space alone.
INSERT INTO asset_kinds (id, name)
    SELECT gen_random_uuid(), name
    FROM (
        SELECT name, row_number() OVER (PARTITION BY lower(name) ORDER BY count(*) DESC, name) AS rank
        FROM (SELECT regexp_replace(kind, '^\s+|\s+$', '', 'g') AS name FROM assets) AS spelled
        GROUP BY name
    ) AS ranked
    WHERE rank = 1;

ALTER TABLE assets ADD COLUMN kind_id uuid REFERENCES asset_kinds (id);

UPDATE assets a SET kind_id = k.id
    FROM asset_kinds k
    WHERE lower(k.name) = lower(regexp_replace(a.kind, '^\s+|\s+$', '', 'g'));

ALTER TABLE assets ALTER COLUMN kind_id SET NOT NULL;
ALTER TABLE assets DROP COLUMN kind;

CREATE INDEX assets_kind ON assets (kind_id);
