-- Canonicalize identities before enforcing case-insensitive uniqueness. If
-- existing rows collapse to the same address, the existing UNIQUE constraint
-- intentionally aborts this migration for manual account reconciliation.
UPDATE users
SET email = lower(btrim(email));

CREATE UNIQUE INDEX idx_users_email_lower_unique
    ON users (lower(email));

ALTER TABLE users
    ADD CONSTRAINT users_email_is_canonical
    CHECK (email = lower(email) AND email = btrim(email));
