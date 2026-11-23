-- Administering how agents sign in (docs/domain-rules.md, "Signing in"; ADR 0008).

-- When the agent last signed in: the last session opened for them, with a local password or
-- through the identity provider. Signing out, or a session ending, leaves it as it is.
ALTER TABLE agents ADD COLUMN last_signed_in_at timestamptz;
UPDATE agents a SET last_signed_in_at = s.last
FROM (SELECT agent_id, max(created_at) AS last FROM agent_sessions GROUP BY agent_id) s
WHERE s.agent_id = a.id;

-- Every session opened is a sign-in. The trigger is named to come before the other triggers on
-- new sessions (agent_sessions_for_active_agents reads the agent under a share lock): updating
-- the agent first takes the row's write lock at once, so that two sign-ins of the same agent at
-- the same moment wait for each other instead of each holding a share lock the other's update
-- needs. A session the next triggers refuse takes the update back with it.
CREATE FUNCTION sessions_record_the_sign_in() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    UPDATE agents SET last_signed_in_at = NEW.created_at WHERE id = NEW.agent_id;
    RETURN NEW;
END;
$$;

CREATE TRIGGER agent_sessions_are_sign_ins
    BEFORE INSERT ON agent_sessions
    FOR EACH ROW EXECUTE FUNCTION sessions_record_the_sign_in();

-- Unlinking an agent's identity ends the sessions opened through the identity provider.
CREATE FUNCTION identity_unlinked() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM agent_sessions WHERE agent_id = OLD.agent_id AND opened_with = 'openid_connect';
    RETURN NULL;
END;
$$;

CREATE TRIGGER agent_identities_unlinked_end_sessions
    AFTER DELETE ON agent_identities
    FOR EACH ROW EXECUTE FUNCTION identity_unlinked();

-- Removing an agent's local password ends the sessions opened with it.
CREATE FUNCTION local_password_removed() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM agent_sessions WHERE agent_id = NEW.id AND opened_with = 'password';
    RETURN NULL;
END;
$$;

CREATE TRIGGER agents_local_password_removed_ends_sessions
    AFTER UPDATE OF password_hash ON agents
    FOR EACH ROW WHEN (OLD.password_hash IS NOT NULL AND NEW.password_hash IS NULL)
    EXECUTE FUNCTION local_password_removed();
