-- IPv4-mapped IPv6 socket addresses represent individual IPv4 clients, not an
-- IPv6 allocation. Convert them first so exact IPv4 moderation is preserved.
UPDATE room_states
SET ip_address = '0.0.0.0'::inet
    + (ip_address - '::ffff:0.0.0.0'::inet)
WHERE user_id IS NULL
  AND ip_address IS NOT NULL
  AND family(ip_address) = 6
  AND ip_address <<= '::ffff:0.0.0.0/96'::inet;

-- Guest moderation treats a native IPv6 /64 as one durable identity.
-- Canonicalize sanctions written by older binaries before enforcing that
-- identity at the database layer. Native IPv4 rows remain unchanged.
UPDATE room_states
SET ip_address = host(network(set_masklen(ip_address, 64)))::inet
WHERE user_id IS NULL
  AND ip_address IS NOT NULL
  AND family(ip_address) = 6;

-- Canonicalization may merge several historical privacy addresses. Preserve
-- the strongest expiry (indefinite wins, otherwise the latest timestamp)
-- before removing duplicate rows.
WITH merged_expiries AS (
    SELECT
        room_id,
        state,
        ip_address,
        CASE
            WHEN bool_or(expires_at IS NULL) THEN NULL
            ELSE max(expires_at)
        END AS expires_at
    FROM room_states
    WHERE user_id IS NULL
      AND ip_address IS NOT NULL
    GROUP BY room_id, state, ip_address
)
UPDATE room_states AS room_state
SET expires_at = merged_expiries.expires_at
FROM merged_expiries
WHERE room_state.user_id IS NULL
  AND room_state.room_id = merged_expiries.room_id
  AND room_state.state = merged_expiries.state
  AND room_state.ip_address = merged_expiries.ip_address;

WITH duplicate_guest_states AS (
    SELECT
        id,
        row_number() OVER (
            PARTITION BY room_id, state, ip_address
            ORDER BY created_at DESC, id DESC
        ) AS duplicate_number
    FROM room_states
    WHERE user_id IS NULL
      AND ip_address IS NOT NULL
)
DELETE FROM room_states
USING duplicate_guest_states
WHERE room_states.id = duplicate_guest_states.id
  AND duplicate_guest_states.duplicate_number > 1;

CREATE UNIQUE INDEX idx_room_states_guest_ip_state_unique
    ON room_states (room_id, ip_address, state)
    WHERE user_id IS NULL AND ip_address IS NOT NULL;
