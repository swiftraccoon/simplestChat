-- Validate the defaults separately from the column/constraint creation transaction.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';

ALTER TABLE users VALIDATE CONSTRAINT users_profile_style_valid;
ALTER TABLE rooms VALIDATE CONSTRAINT rooms_name_style_valid;
ALTER TABLE rooms VALIDATE CONSTRAINT rooms_topic_style_valid;
