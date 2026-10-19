SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';

-- Room roles use the protocol's single numbering (guest 0, user 1, member 2,
-- moderator 3, admin 4, owner 5) instead of the former 1–3 member/moderator/admin
-- scheme, so one `Role` type encodes the wire, the database and the receipts.
-- `invite_redemptions.granted_role` already used this numbering. The new
-- checks are validated by migration 036 without blocking writes here.
ALTER TABLE room_roles DROP CONSTRAINT room_roles_role_check;
UPDATE room_roles SET role = role + 1;
ALTER TABLE room_roles
    ADD CONSTRAINT room_roles_role_check CHECK (role BETWEEN 2 AND 4) NOT VALID;

ALTER TABLE invites DROP CONSTRAINT invites_role_check;
UPDATE invites SET role = role + 1;
ALTER TABLE invites
    ADD CONSTRAINT invites_role_check CHECK (role BETWEEN 2 AND 4) NOT VALID;
