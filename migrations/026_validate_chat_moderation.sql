-- Validate separately from the transaction that installs the expanded check.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';

ALTER TABLE moderation_events VALIDATE CONSTRAINT moderation_events_action_check;
