-- Key requesters (docs/domain-rules.md, "People"): requesters who speak for their customer. They
-- get the replies on its tickets that have no requester and the messages about failures that
-- affect it, at their contact address, which is always one of their addresses.

ALTER TABLE requester_addresses
    -- Target of the key that keeps a contact address among its requester's addresses.
    ADD CONSTRAINT requester_addresses_of_requester UNIQUE (address, requester_id);

CREATE TABLE key_requesters (
    requester_id     uuid PRIMARY KEY,
    -- A key requester speaks for their own customer: a requester's customer does not change
    -- while they are one.
    customer_id      uuid NOT NULL,
    -- One of the requester's addresses: it is not removed, nor passed to another requester,
    -- while it is their contact address.
    contact_address  text NOT NULL,
    marked_at        timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY (requester_id, customer_id) REFERENCES requesters (id, customer_id),
    FOREIGN KEY (contact_address, requester_id)
        REFERENCES requester_addresses (address, requester_id)
);

CREATE INDEX key_requesters_customer ON key_requesters (customer_id);
