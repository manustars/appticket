-- Signed-in agents. Only a hash of the session token is stored.
CREATE TABLE agent_sessions (
    token_hash  bytea PRIMARY KEY,
    agent_id    uuid NOT NULL REFERENCES agents (id) ON DELETE CASCADE,
    created_at  timestamptz NOT NULL DEFAULT now(),
    expires_at  timestamptz NOT NULL
);

CREATE INDEX agent_sessions_agent ON agent_sessions (agent_id);
