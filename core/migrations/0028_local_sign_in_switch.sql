-- Whether agents can sign in with a local password (docs/domain-rules.md, "Signing in";
-- ADR 0008): kept by the product, not in the deployment, and changed with the command line.

-- The installation's settings: one row, always there.
CREATE TABLE installation_settings (
    only_row       boolean PRIMARY KEY DEFAULT true CHECK (only_row),
    -- Whether agents can sign in with a local password. Switched off, every session opened with
    -- one ends, and none is opened until it is switched back on.
    local_sign_in  boolean NOT NULL DEFAULT true
);

INSERT INTO installation_settings DEFAULT VALUES;

CREATE FUNCTION installation_settings_stay() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'the installation''s settings are changed, not removed'
        USING ERRCODE = 'restrict_violation';
END;
$$;

CREATE TRIGGER installation_settings_not_deleted
    BEFORE DELETE ON installation_settings
    FOR EACH ROW EXECUTE FUNCTION installation_settings_stay();

CREATE TRIGGER installation_settings_not_truncated
    BEFORE TRUNCATE ON installation_settings
    FOR EACH STATEMENT EXECUTE FUNCTION installation_settings_stay();

-- Switching local sign-in off ends every session opened with a local password, and keeps those
-- opened through the identity provider.
CREATE FUNCTION local_sign_in_switched_off() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM agent_sessions WHERE opened_with = 'password';
    RETURN NULL;
END;
$$;

CREATE TRIGGER local_sign_in_off_ends_password_sessions
    AFTER UPDATE OF local_sign_in ON installation_settings
    FOR EACH ROW WHEN (NOT NEW.local_sign_in)
    EXECUTE FUNCTION local_sign_in_switched_off();

-- No session is opened with a local password while local sign-in is off. The setting is read
-- under a share lock, so that a sign-in and the switch at the same moment take turns: a sign-in
-- that comes second is refused, a switch that comes second ends the session.
CREATE FUNCTION password_sessions_only_with_local_sign_in() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.opened_with = 'password'
       AND NOT (SELECT local_sign_in FROM installation_settings FOR SHARE) THEN
        RAISE EXCEPTION 'local sign-in is off'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'local_sign_in_on';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER agent_sessions_with_password_only_with_local_sign_in
    BEFORE INSERT OR UPDATE OF opened_with ON agent_sessions
    FOR EACH ROW EXECUTE FUNCTION password_sessions_only_with_local_sign_in();
