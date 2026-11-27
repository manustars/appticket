-- An add-on resolves the tickets it opened (docs/domain-rules.md, "Add-ons"): `ticket.state_changed`
-- may now say an add-on changed the state, by its name, and only to resolve the ticket; and a
-- ticket keeps whether an agent has worked on it, which the add-on reads first.
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

-- Whether an agent has worked on the ticket, which an add-on weighs before resolving a ticket it
-- opened: once they have been its assignee, written a reply, a note or an announcement on it, or
-- changed it by hand (its state, priority, links, customer or parent). Once set it stays set, so
-- that unassigning or anything later never makes a ticket look untouched. The database sets it
-- for what leaves a trace of the agent (an assignee, an agent's note or message, an
-- announcement); the core sets it, in the same transaction, for the changes an agent makes by
-- hand.
ALTER TABLE tickets ADD COLUMN worked_on_by_agent boolean NOT NULL DEFAULT false;

UPDATE tickets t SET worked_on_by_agent = true
WHERE t.assignee_id IS NOT NULL
   OR EXISTS (SELECT 1 FROM messages m WHERE m.ticket_id = t.id AND m.author_id IS NOT NULL)
   OR EXISTS (SELECT 1 FROM notes n WHERE n.ticket_id = t.id AND n.author_id IS NOT NULL)
   OR EXISTS (SELECT 1 FROM announcements a WHERE a.ticket_id = t.id)
   -- Only agents attach a child to a parent.
   OR t.parent_id IS NOT NULL
   OR EXISTS (SELECT 1 FROM tickets c WHERE c.parent_id = t.id)
   OR EXISTS (SELECT 1 FROM events e
              WHERE e.kind = 'ticket.state_changed'
                AND e.data -> 'changed_by' ->> 'kind' = 'agent'
                AND (e.data ->> 'ticket_number')::bigint = t.number);

CREATE FUNCTION tickets_stay_worked_on() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.worked_on_by_agent := NEW.worked_on_by_agent OR NEW.assignee_id IS NOT NULL
        OR (TG_OP = 'UPDATE' AND OLD.worked_on_by_agent);
    RETURN NEW;
END;
$$;

CREATE TRIGGER tickets_stay_worked_on
    BEFORE INSERT OR UPDATE ON tickets
    FOR EACH ROW EXECUTE FUNCTION tickets_stay_worked_on();

-- An agent's note or message, and an announcement, which agents always write, mark the ticket.
CREATE FUNCTION agents_work_on_tickets() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.author_id IS NOT NULL THEN
        UPDATE tickets SET worked_on_by_agent = true
        WHERE id = NEW.ticket_id AND NOT worked_on_by_agent;
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER agents_notes_work_on_tickets
    AFTER INSERT ON notes
    FOR EACH ROW EXECUTE FUNCTION agents_work_on_tickets();

CREATE TRIGGER agents_messages_work_on_tickets
    AFTER INSERT ON messages
    FOR EACH ROW EXECUTE FUNCTION agents_work_on_tickets();

CREATE TRIGGER announcements_work_on_tickets
    AFTER INSERT ON announcements
    FOR EACH ROW EXECUTE FUNCTION agents_work_on_tickets();
