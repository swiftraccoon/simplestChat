# E2E Verification Suite

`checklist.cjs` verifies the full client feature set against a deployed server by driving
two Chromium instances (a registered room owner and a guest) through 27 checks:

- Guest join, login modal, register, logout/login, room browser, room creation
- Password rooms: owner bypass, wrong password rejected, prompt-and-retry flow
- Lobby: waiting screen, moderator admit/deny panel, post-admission media setup
- Moderation: all context-menu actions, role-hierarchy gating, voice request grant
- Kick/ban alerts, topic display + click-to-edit, complete settings modal
- Persistent bans: guest (IP) ban survives reconnect; registered-user ban survives
  room teardown and re-login
- Live settings enforcement (chat/screen/camera toggles propagate to participants)
- Real bidirectional WebRTC video (`videoWidth > 0` on both sides)
- Active-speaker highlighting from server-side audio observers
- Moderated-room produce denial for unvoiced users

## Requirements

- The origin must be a **secure context** for `getUserMedia`. Against a remote
  server, tunnel it to localhost: `ssh -f -N -L 3100:localhost:3000 <server>`
- Media flows directly to the server's `ANNOUNCE_IP` over UDP (tunnel carries only
  HTTP/WS), so the test machine must reach the server's RTC ports.
- The server needs `DATABASE_URL` and `JWT_SECRET` set (auth checks fail otherwise).

## Run

```bash
npm install playwright        # in this directory (or any parent)
node checklist.cjs            # targets http://localhost:3100
```

Each run uses unique room IDs/emails (timestamp-suffixed) so it is rerunnable, but it
does leave `e2e-*` rooms, users, and ban rows in the database. Cleanup (rooms first —
`room_states.applied_by` references users without cascade):

```sql
DELETE FROM rooms WHERE id LIKE 'e2e-%';
DELETE FROM users WHERE email LIKE 'e2e-%@test.local';
```

Note: the ban checks record IP bans against the test machine's IP (127.0.0.1 through
the SSH tunnel) scoped to that run's unique room — they do not affect other rooms.

Failures write screenshots and `results.json` (plus per-page console logs) to `artifacts/`.

## History

This suite was built to close out the 2026-02-18 client-completion plan's manual test
checklist. Its first full run caught five real bugs: the `RoomSettings` snake_case wire
format, the never-cleared connection lobby flag, the room-screen-while-lobbied UI state,
the per-join topic listener leak, and the empty-cname consume failure on newer Chromium.
