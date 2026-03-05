# ArcTracker Bug Report

## Title
`/api/v2/user/quests`, `/api/v2/user/hideout`, and `/api/v2/user/projects` return HTTP 500 with valid dual-key auth while other user endpoints succeed

## Body
I’m seeing consistent `500 internal_error` responses on specific authenticated user endpoints, even though authentication is valid and other user endpoints work.

### Summary
- Base URL: `https://arctracker.io`
- Auth used exactly per docs:
  - `X-App-Key: arc_k1_...`
  - `Authorization: Bearer arc_u1_...`
- Endpoints that succeed (`200`):
  - `/api/v2/user/profile?locale=en`
  - `/api/v2/user/stash?locale=en&page=1&per_page=50`
  - `/api/v2/user/loadout?locale=en`
- Endpoints that fail (`500`):
  - `/api/v2/user/quests?locale=en&filter=incomplete`
  - `/api/v2/user/quests?locale=en&filter=completed`
  - `/api/v2/user/hideout?locale=en`
  - `/api/v2/user/projects?locale=en&season=1`
  - `/api/v2/user/projects?locale=en&season=2`

### Expected behavior
These endpoints should return user progress data (or a documented auth/scope error like `403` if there is a permission issue).

### Actual behavior
Consistent `500` responses with:

```json
{"error":{"code":"internal_error","message":"Internal server error"},"meta":{"requestId":"..."}}
```

### Repro steps
1. Send request with valid app key + user key using documented headers.
2. Call any of the failing endpoints above.
3. Observe HTTP `500`.

### Additional notes
- Requests are throttled to about `1 req/sec` and retried with backoff; failures persist.
- Raw requests outside the app reproduce the same behavior.

## Diagnostics Dump
```text
ArcTracker API diagnostics: 3 passed, 5 failed
[OK] /api/v2/user/profile?locale=en status=200 requestId=f6d8a6b2-4cfd-4d87-90b5-3cb659e4bd57 detail=OK
[OK] /api/v2/user/stash?locale=en&page=1&per_page=50&sort=slot status=200 requestId=26e46015-9ec9-4d39-bcff-c56a3c5506ae detail=OK
[OK] /api/v2/user/loadout?locale=en status=200 requestId=5caa0e12-f505-4cf0-a692-8210961e2f3f detail=OK
[ERR] /api/v2/user/quests?locale=en&filter=incomplete status=500 requestId=c63439e5-846b-4e91-962a-a8375dddd912 detail=HTTP 500 Internal Server Error (requestId=c63439e5-846b-4e91-962a-a8375dddd912): {"error":{"code":"internal_error","message":"Internal server error"},"meta":{"requestId":"c63439e5-846b-4e91-962a-a8375dddd912"}}
[ERR] /api/v2/user/quests?locale=en&filter=completed status=500 requestId=720e6529-e64b-4caf-a4f1-29d355192059 detail=HTTP 500 Internal Server Error (requestId=720e6529-e64b-4caf-a4f1-29d355192059): {"error":{"code":"internal_error","message":"Internal server error"},"meta":{"requestId":"720e6529-e64b-4caf-a4f1-29d355192059"}}
[ERR] /api/v2/user/hideout?locale=en status=500 requestId=a5aaaade-d829-445d-84cf-45bfb552f0d0 detail=HTTP 500 Internal Server Error (requestId=a5aaaade-d829-445d-84cf-45bfb552f0d0): {"error":{"code":"internal_error","message":"Internal server error"},"meta":{"requestId":"a5aaaade-d829-445d-84cf-45bfb552f0d0"}}
[ERR] /api/v2/user/projects?locale=en&season=1 status=500 requestId=f5951d2e-46b2-4bb7-8ed4-750cfc3f28ff detail=HTTP 500 Internal Server Error (requestId=f5951d2e-46b2-4bb7-8ed4-750cfc3f28ff): {"error":{"code":"internal_error","message":"Internal server error"},"meta":{"requestId":"f5951d2e-46b2-4bb7-8ed4-750cfc3f28ff"}}
[ERR] /api/v2/user/projects?locale=en&season=2 status=500 requestId=af8bedab-81e0-440d-83c5-9ed95fb211c1 detail=HTTP 500 Internal Server Error (requestId=af8bedab-81e0-440d-83c5-9ed95fb211c1): {"error":{"code":"internal_error","message":"Internal server error"},"meta":{"requestId":"af8bedab-81e0-440d-83c5-9ed95fb211c1"}}
```
