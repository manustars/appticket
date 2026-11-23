-- The local password is optional (docs/domain-rules.md, "Signing in"; ADR 0008): agents created
-- by a first sign-in through an identity provider have none, and existing agents keep theirs.
-- No local password is no hash at all, never an empty one.
ALTER TABLE agents
    ALTER COLUMN password_hash DROP NOT NULL,
    ADD CONSTRAINT agents_password_hash_not_empty CHECK (password_hash <> '');
