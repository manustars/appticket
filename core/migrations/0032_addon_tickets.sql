-- Tickets that add-ons open and the notes they add (docs/domain-rules.md, "Add-ons").

-- The add-on that opened a ticket, when an add-on did. An add-on opens tickets without a
-- requester, and nothing gives an existing ticket one.
ALTER TABLE tickets
    ADD COLUMN opened_by_addon_id uuid REFERENCES addons (id),
    ADD CONSTRAINT tickets_opened_by_an_addon_have_no_requester
        CHECK (opened_by_addon_id IS NULL OR requester_id IS NULL);

-- A note is written by an agent or by an add-on: exactly one of them.
ALTER TABLE notes
    ALTER COLUMN author_id DROP NOT NULL,
    ADD COLUMN addon_id uuid REFERENCES addons (id),
    ADD CONSTRAINT notes_have_one_author CHECK ((author_id IS NULL) <> (addon_id IS NULL));
