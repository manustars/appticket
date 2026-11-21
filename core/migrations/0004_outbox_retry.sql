-- When the outbox tries a failed outbound message again; empty means as soon as possible.
ALTER TABLE messages ADD COLUMN next_attempt_at timestamptz;
