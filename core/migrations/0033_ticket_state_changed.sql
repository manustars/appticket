-- The event ticket.state_changed (sdk/README.md): recorded only for a real change of a ticket's
-- state, which never leaves a closed ticket (docs/domain-rules.md, "Tickets"), and always saying
-- who or what changed it.
ALTER TABLE events
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
            )
        ) IS TRUE
    );
