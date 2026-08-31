-- Leading-wildcard ILIKE searches cannot use ordinary B-tree indexes. A
-- partial trigram index keeps public room-name and topic searches bounded
-- without indexing secret-room metadata.
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Search the same newline-separated expression in the application. Search
-- queries reject control characters, so a match cannot cross the separator
-- and still has the original "display name OR topic" semantics.
CREATE INDEX IF NOT EXISTS idx_rooms_public_search_trgm
    ON rooms USING GIN (
        (display_name || E'\n' || COALESCE(topic, '')) gin_trgm_ops
    )
    WITH (fastupdate = off)
    WHERE secret = false;
