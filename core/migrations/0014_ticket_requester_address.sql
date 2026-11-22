-- The address that stands for a ticket's requester is the one a reply goes to (docs/domain-rules.md,
-- "People"), so that the ticket and its lists show where the conversation goes.

-- The address an agent's last reply on the ticket went to, while it is still the requester's: on a
-- ticket they have not written on, later replies go there unless the agent picks another.
CREATE FUNCTION ticket_picked_address(ticket uuid) RETURNS text LANGUAGE sql STABLE AS $$
    SELECT m.to_address
    FROM tickets t
    JOIN messages m ON m.ticket_id = t.id AND m.direction = 'outbound'
    JOIN requester_addresses a ON a.address = m.to_address AND a.requester_id = t.requester_id
    WHERE t.id = ticket
    ORDER BY m.created_at DESC, m.id
    LIMIT 1
$$;

-- The address that stands for the ticket's requester: where they last wrote from on it, else the
-- one an agent last picked there, else the first they registered. Null without a requester.
CREATE FUNCTION ticket_requester_address(ticket uuid) RETURNS text LANGUAGE sql STABLE AS $$
    SELECT coalesce(ticket_reply_address(t.id), ticket_picked_address(t.id),
                    requester_first_address(t.requester_id))
    FROM tickets t
    WHERE t.id = ticket
$$;
