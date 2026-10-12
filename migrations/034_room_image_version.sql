-- Listings name a room image by a digest instead of carrying its bytes, and
-- the image endpoint answers only the exact current digest. Binding the room
-- id into the digest keeps identical pictures in two rooms distinguishable.
-- The identity update writes the column through the same function, so there
-- is one definition of the digest; convert_to is STABLE (server encoding) and
-- deterministic in this UTF-8 database.
SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '10min';
CREATE FUNCTION room_image_version(room_id TEXT, image TEXT) RETURNS TEXT
    LANGUAGE sql STABLE PARALLEL SAFE RETURNS NULL ON NULL INPUT
    AS $$ SELECT left(encode(sha256(convert_to(room_id || E'\n' || image, 'UTF8')), 'hex'), 16) $$;
GRANT EXECUTE ON FUNCTION room_image_version(TEXT, TEXT) TO PUBLIC;
ALTER TABLE rooms ADD COLUMN image_version TEXT;
UPDATE rooms SET image_version = room_image_version(id, image_url) WHERE image_url IS NOT NULL;
