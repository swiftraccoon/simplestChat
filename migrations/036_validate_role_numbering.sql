SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';

-- Validate the role checks migration 035 added NOT VALID.
ALTER TABLE room_roles VALIDATE CONSTRAINT room_roles_role_check;
ALTER TABLE invites VALIDATE CONSTRAINT invites_role_check;
