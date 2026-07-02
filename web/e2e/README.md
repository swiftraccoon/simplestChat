# E2E Verification Suite

`checklist.cjs` verifies the full client feature set against a deployed server by driving
two Chromium instances (a registered room owner and a guest) through 22 checks:

- Guest join, login modal, register, logout/login, room browser, room creation
- Lobby: waiting screen, moderator admit/deny panel, post-admission media setup
- Moderation: all context-menu actions, role-hierarchy gating, voice request grant
- Kick/ban alerts, topic display + click-to-edit, complete settings modal
- Live settings enforcement (chat/screen/camera toggles propagate to participants)
- Real bidirectional WebRTC video (`videoWidth > 0` on both sides)
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
does leave `e2e-*` rooms and users in the database. Cleanup:

```sql
DELETE FROM rooms WHERE id LIKE 'e2e-%';
DELETE FROM users WHERE email LIKE 'e2e-%@test.local';
```

Failures write screenshots and `results.json` (plus per-page console logs) to `artifacts/`.

## History

This suite was built to close out the 2026-02-18 client-completion plan's manual test
checklist. Its first full run caught five real bugs: the `RoomSettings` snake_case wire
format, the never-cleared connection lobby flag, the room-screen-while-lobbied UI state,
the per-join topic listener leak, and the empty-cname consume failure on newer Chromium.
