-- WebAuthn requires credential IDs to be globally unique across accounts.
-- Existing Passkey JSON serializes this value at cred.cred_id.
ALTER TABLE webauthn_credentials
    ADD COLUMN credential_id TEXT;

UPDATE webauthn_credentials
SET credential_id = credential_json #>> '{cred,cred_id}';

ALTER TABLE webauthn_credentials
    ALTER COLUMN credential_id SET NOT NULL;

CREATE UNIQUE INDEX idx_webauthn_credentials_credential_id_unique
    ON webauthn_credentials (credential_id);
