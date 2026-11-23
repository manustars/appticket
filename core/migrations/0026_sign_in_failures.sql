-- Failed sign-ins (docs/domain-rules.md, "Signing in"), counted per account and per address so
-- that repeated failures are slowed and then refused for a while. An attempt is recorded here
-- before its password is checked and removed when the password turns out right, so that attempts
-- still being checked count too. Rows older than two windows are deleted as new ones come.

CREATE TABLE sign_in_failures (
    id         uuid PRIMARY KEY,
    -- What the person signed in as, trimmed and in lower case: an agent's e-mail, or any text
    -- typed in, whether or not an agent has it (its SHA-256 when longer than any e-mail). Null
    -- once someone signed in as that account: the failure was theirs, and counts for the address
    -- only.
    account    text,
    -- The client's address: an IPv4 address, or the /64 network of an IPv6 address.
    address    text NOT NULL CHECK (address <> ''),
    failed_at  timestamptz NOT NULL
);

CREATE INDEX sign_in_failures_account ON sign_in_failures (account, failed_at);
CREATE INDEX sign_in_failures_address ON sign_in_failures (address, failed_at);
-- For deleting the old ones without reading the whole table.
CREATE INDEX sign_in_failures_failed_at ON sign_in_failures (failed_at);
