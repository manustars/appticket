-- The names of an add-on's system that customers' external references are chosen from
-- (docs/domain-rules.md, "Add-ons"), such as a monitoring tool's host groups, with their nesting:
-- the add-on sends the whole list, under the name of its system, and the core keeps the latest one,
-- so that agents choose from it when the add-on is switched off too. External references never
-- depend on the list: a name that leaves it, or one typed by hand, stays a customer's reference.

CREATE TABLE name_lists (
    addon       text PRIMARY KEY REFERENCES addons (name),
    -- The add-on's system, as external references name it: trimmed, not blank, without control
    -- characters and short enough to be read in a list.
    system      text NOT NULL CHECK (system ~ '^\S(.*\S)?$' AND system !~ '[[:cntrl:]]'
                                     AND char_length(system) <= 100),
    updated_at  timestamptz NOT NULL DEFAULT now()
);

-- A system's names come from one add-on, compared ignoring case as external references are.
CREATE UNIQUE INDEX name_lists_system ON name_lists (lower(system));

CREATE TABLE listed_names (
    id            uuid PRIMARY KEY,
    addon         text NOT NULL REFERENCES name_lists (addon) ON DELETE CASCADE,
    -- Trimmed, not blank, without control characters and short enough to be read in a list.
    name          text NOT NULL CHECK (name ~ '^\S(.*\S)?$' AND name !~ '[[:cntrl:]]'
                                       AND char_length(name) <= 255),
    -- The name it is nested in, of the same list, one level up: the depth falls by one at each
    -- step towards the top, so that no name is nested in itself, however far up.
    parent_id     uuid,
    depth         integer NOT NULL CHECK (depth >= 0),
    parent_depth  integer,
    CHECK ((parent_id IS NULL AND parent_depth IS NULL AND depth = 0)
           OR (parent_id IS NOT NULL AND parent_depth = depth - 1)),
    UNIQUE (addon, id, depth),
    FOREIGN KEY (addon, parent_id, parent_depth) REFERENCES listed_names (addon, id, depth)
);

-- A name appears once in its list, ignoring case. The names are told apart byte by byte ("C"),
-- which finds the same names equal as the database's own collation does, since that one is
-- deterministic, but costs the same for any character: some locales take milliseconds to order
-- two long strings of characters they give no weight to, such as emoji, and an add-on sends
-- thousands of names at once.
CREATE UNIQUE INDEX listed_names_name ON listed_names (addon, (lower(name) COLLATE "C"));
