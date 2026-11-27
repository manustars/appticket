-- An add-on resolves the tickets it opened (docs/domain-rules.md, "Add-ons"): `ticket.state_changed`
-- may now say an add-on changed the state, by its name, and only to resolve the ticket.
ALTER TABLE events
    DROP CONSTRAINT events_ticket_state_changed_is_a_change,
    ADD CONSTRAINT events_ticket_state_changed_is_a_change CHECK (
        kind <> 'ticket.state_changed' OR (
            jsonb_typeof(data -> 'ticket_number') = 'number'
            AND data ->> 'old_state' IN ('open', 'waiting', 'resolved')
            AND data ->> 'new_state' IN ('open', 'waiting', 'resolved', 'closed')
            AND data ->> 'old_state' <> data ->> 'new_state'
            AND (
                (data -> 'changed_by' ->> 'kind' = 'agent'
                    AND jsonb_typeof(data -> 'changed_by' -> 'agent' -> 'id') = 'string')
                OR (data -> 'changed_by' ->> 'kind' = 'message'
                    AND jsonb_typeof(data -> 'changed_by' -> 'message_id') = 'string')
                OR (data -> 'changed_by' ->> 'kind' = 'addon'
                    AND jsonb_typeof(data -> 'changed_by' -> 'addon') = 'string'
                    AND data ->> 'new_state' = 'resolved')
            )
        ) IS TRUE
    );
