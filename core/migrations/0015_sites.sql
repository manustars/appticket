-- A customer's sites (docs/domain-rules.md, "Customers, assets and services"): each with a name and
-- an address, retired and never deleted.

CREATE TABLE sites (
    id           uuid PRIMARY KEY,
    customer_id  uuid NOT NULL REFERENCES customers (id),
    -- Trimmed and not blank: no white space, line ends included, at either end.
    name         text NOT NULL CHECK (name ~ '^\S(.*\S)?$'),
    address      text NOT NULL CHECK (address ~ '^\S(.*\S)?$'),
    created_at   timestamptz NOT NULL DEFAULT now(),
    -- A retired site stays visible but gets no new requesters or assets.
    retired_at   timestamptz,
    -- Target of the keys that will keep a requester's or an asset's site inside its customer.
    UNIQUE (id, customer_id)
);

-- Sites are named, in imports too: a customer's active sites have different names.
CREATE UNIQUE INDEX sites_active_name ON sites (customer_id, lower(name)) WHERE retired_at IS NULL;
