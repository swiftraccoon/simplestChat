ALTER TABLE users
    ADD COLUMN avatar_url TEXT,
    ADD COLUMN bio TEXT NOT NULL DEFAULT '',
    ADD COLUMN recovery_key_hash VARCHAR(64),
    ADD COLUMN auth_version BIGINT NOT NULL DEFAULT 0,
    ADD CONSTRAINT users_bio_length CHECK (octet_length(bio) <= 1024),
    ADD CONSTRAINT users_avatar_length CHECK (avatar_url IS NULL OR octet_length(avatar_url) <= 175000),
    ADD CONSTRAINT users_recovery_hash_format CHECK (recovery_key_hash IS NULL OR recovery_key_hash ~ '^[0-9a-f]{64}$');

ALTER TABLE rooms
    ADD COLUMN description TEXT NOT NULL DEFAULT '',
    ADD COLUMN image_url TEXT,
    ADD CONSTRAINT rooms_description_length CHECK (octet_length(description) <= 1024),
    ADD CONSTRAINT rooms_image_length CHECK (image_url IS NULL OR octet_length(image_url) <= 175000);
