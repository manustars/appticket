-- A customer's external references (docs/domain-rules.md, "Customers, assets and services"): each
-- a system and a name in it, such as a group in a monitoring tool, that add-ons read to tell which
-- customer something of their system belongs to.

CREATE TABLE external_references (
    id           uuid PRIMARY KEY,
    customer_id  uuid NOT NULL REFERENCES customers (id),
    -- Trimmed and not blank: no white space, line ends included, at either end.
    system       text NOT NULL CHECK (system ~ '^\S(.*\S)?$'),
    name         text NOT NULL CHECK (name ~ '^\S(.*\S)?$'),
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- A name names exactly one customer within its system, ignoring case, so that two spellings of
-- one group never point at two customers. It holds for offboarded customers too, who can come
-- back with their references.
CREATE UNIQUE INDEX external_references_name ON external_references (lower(system), lower(name));
CREATE INDEX external_references_customer ON external_references (customer_id);
