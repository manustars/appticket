-- Signing in through an identity provider (docs/domain-rules.md, "Signing in"; ADR 0008).

-- How each session was opened: with a local password, or through the OpenID Connect identity
-- provider. The sessions there are already were all opened with a password.
ALTER TABLE agent_sessions
    ADD COLUMN opened_with text NOT NULL DEFAULT 'password'
        CHECK (opened_with IN ('password', 'openid_connect'));
ALTER TABLE agent_sessions ALTER COLUMN opened_with DROP DEFAULT;
