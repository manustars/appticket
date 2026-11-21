-- Moving a requester to another customer moves all of the requester's tickets with them: the
-- identification was wrong for all of them (docs/domain-rules.md, "Tickets").
ALTER TABLE tickets DROP CONSTRAINT tickets_requester_id_customer_id_fkey;
ALTER TABLE tickets
    ADD CONSTRAINT tickets_requester_id_customer_id_fkey
    FOREIGN KEY (requester_id, customer_id) REFERENCES requesters (id, customer_id)
    ON UPDATE CASCADE;
