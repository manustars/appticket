-- Signing in through an identity provider (docs/domain-rules.md, "Signing in"; ADR 0008).

-- How each session was opened: with a local password, or through the OpenID Connect identity
-- provider. The sessions there are already were all opened with a password.
ALTER TABLE agent_sessions
    ADD COLUMN opened_with text NOT NULL DEFAULT 'password'
        CHECK (opened_with IN ('password', 'openid_connect'));
ALTER TABLE agent_sessions ALTER COLUMN opened_with DROP DEFAULT;

-- An identity at an identity provider, linked to the agent it signs in as: linked by verified
-- e-mail the first time, found by the provider's stable identifier from then on. An agent has at
-- most one identity per provider.
CREATE TABLE agent_identities (
    -- The provider: an OpenID Connect issuer URL.
    issuer     text NOT NULL CHECK (btrim(issuer) <> ''),
    -- The provider's stable identifier of the person (OpenID Connect: the `sub` claim).
    subject    text NOT NULL CHECK (subject <> ''),
    agent_id   uuid NOT NULL REFERENCES agents (id) ON DELETE CASCADE,
    linked_at  timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (issuer, subject),
    UNIQUE (agent_id, issuer)
);

-- A deactivated agent is neither linked nor signed in: signing in never reactivates them. The
-- agent is locked, so that a deactivation at the same time is seen or waits for this.
CREATE FUNCTION only_active_agents_sign_in() RETURNS trigger
    LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM agents
                   WHERE id = NEW.agent_id AND deactivated_at IS NULL FOR SHARE) THEN
        RAISE EXCEPTION 'a deactivated agent cannot sign in'
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER agent_sessions_for_active_agents
    BEFORE INSERT ON agent_sessions
    FOR EACH ROW EXECUTE FUNCTION only_active_agents_sign_in();

CREATE TRIGGER agent_identities_for_active_agents
    BEFORE INSERT OR UPDATE OF agent_id ON agent_identities
    FOR EACH ROW EXECUTE FUNCTION only_active_agents_sign_in();

-- A sign-in through the OpenID Connect identity provider under way, for a few minutes: what the
-- browser's return from the provider is checked against, once. Only a hash of the state is kept;
-- the state itself travels in the provider's redirect and in a cookie of the browser that set out.
-- Anyone can set a sign-in out, so an address (as failed sign-ins count it) has a bounded number
-- under way.
CREATE TABLE identity_provider_sign_ins (
    state_hash     bytea PRIMARY KEY,
    nonce          text NOT NULL,
    pkce_verifier  text NOT NULL,
    address        text NOT NULL,
    expires_at     timestamptz NOT NULL
);

CREATE INDEX identity_provider_sign_ins_address ON identity_provider_sign_ins (address, expires_at);
CREATE INDEX identity_provider_sign_ins_expiry ON identity_provider_sign_ins (expires_at);
