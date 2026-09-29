-- Chat preferences that follow an account across devices: the browser's own
-- object (private-message opt-in, sounds, text size, timestamp format and the
-- ignore list), validated by the server before it is stored.
ALTER TABLE users
    ADD COLUMN preferences JSONB NOT NULL DEFAULT '{}'::jsonb,
    ADD CONSTRAINT users_preferences_object CHECK (jsonb_typeof(preferences) = 'object'),
    ADD CONSTRAINT users_preferences_size CHECK (octet_length(preferences::text) <= 16384);
