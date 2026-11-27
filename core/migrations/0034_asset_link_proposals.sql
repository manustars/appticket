-- Link proposals (docs/domain-rules.md, "Add-ons"; ADR 0002): an add-on that would supply an asset
-- its customer already has, by name or serial number, gets a proposal that its asset is that one
-- instead of a second asset. Agents who manage the catalog discard it; a discarded proposal stays,
-- so that the add-on's next attempt with that reference creates its own asset.

CREATE TABLE asset_link_proposals (
    id             uuid PRIMARY KEY,
    -- The add-on that would supply the asset, and the reference it knows it by.
    addon          text NOT NULL REFERENCES addons (name),
    reference      text NOT NULL CHECK (reference ~ '^\S(.*\S)?$'),
    customer_id    uuid NOT NULL REFERENCES customers (id),
    -- The asset of the same customer it matched, and by what.
    asset_id       uuid NOT NULL,
    matched_by     text NOT NULL CHECK (matched_by IN ('serial_number', 'name')),
    -- The name and serial number the add-on sent, for the agents to compare.
    name           text NOT NULL CHECK (btrim(name) <> ''),
    serial_number  text,
    state          text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'discarded')),
    created_at     timestamptz NOT NULL DEFAULT now(),
    decided_at     timestamptz,
    decided_by     uuid REFERENCES agents (id),
    FOREIGN KEY (asset_id, customer_id) REFERENCES assets (id, customer_id),
    -- An open proposal is undecided; a decided one says when and by whom.
    CHECK ((state = 'open') = (decided_at IS NULL)),
    CHECK ((state = 'open') = (decided_by IS NULL))
);

-- At most one open proposal for a reference of an add-on: sending the same asset again records no
-- second one.
CREATE UNIQUE INDEX asset_link_proposals_one_open ON asset_link_proposals (addon, reference)
    WHERE state = 'open';

-- The open proposals on a customer's assets.
CREATE INDEX asset_link_proposals_open_of_customer ON asset_link_proposals (customer_id)
    WHERE state = 'open';
