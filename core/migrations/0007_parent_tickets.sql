-- A failure that affects several customers is tracked by a parent ticket of the help desk's own
-- organization; each customer's ticket about it is a child (docs/domain-rules.md, "Tickets").
ALTER TABLE tickets
    ADD COLUMN parent_id uuid REFERENCES tickets (id),
    ADD CONSTRAINT tickets_not_own_parent CHECK (parent_id <> id);

CREATE INDEX tickets_parent ON tickets (parent_id) WHERE parent_id IS NOT NULL;
