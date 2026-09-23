# Missing preparation evidence: 35482064188

Source: `426851cfe482b3ef4a18a290258c259544f63051`. The controller reached its 20,100-second result deadline. Worker `i-0b1de1a841fcb84e3` terminated; exact-worker cleanup succeeded. No result archive or Actions artifact was returned. Local console recovery was denied. The worker's last build stage and cause of termination remain unknown.

## Confirmed control-plane gaps

- The worker's old finish trap ignored tar and PUT failures, and was installed too late to cover early bootstrap failures.
- After capability rotation the controller retained artifact-role credentials for subsequent EC2 console reads. That role is not the dispatch role with EC2 inspection permission. This is a reproduced credential-lifecycle bug, not proof of the worker's underlying failure.
- The controller did not observe worker lifecycle state and did not retain diagnostics independently of the result archive.

## Repair

- Install early worker exit handling; emit allowlisted lifecycle stages. Preserve original failures, make delivery errors nonzero, sanitize credential files, and drain logs before making the archive. Bound tar, upload, and background-process shutdown.
- Emit preparation heartbeats with checkpoint counts, dependency state, build-log activity/age, disk space, available memory, and load. Never emit raw build-log contents or capability URLs through this channel.
- Use original dispatch credentials only for EC2 reads; keep artifact capabilities rotating. Retain sanitized controller evidence even when AWS reads fail or the archive is absent. A confirmed unavailable worker gets two additional archive-race polls before failure. Unknown/denied reads do not imply worker death.
- Preserve the existing preparation and controller deadlines. A complete result archive and full-50 readiness/catalog validation remain mandatory; telemetry alone is not a success signal.
- Compress the audited worker inside the bootstrap wrapper to stay within the existing EC2 user-data size limit; execute the decoded script only after successful decoding. Forward configuration explicitly across the child-shell boundary.

## Performance interpretation

Five hours is a safety budget, not a measured healthy preparation duration. The preceding instrumented failure spent 3h08m51.699s building; 78.61% was two exclusive failing straggler tails. Removing those failures does not establish a runtime forecast: successful dependencies, readiness, and publication still need measurement. This retry must produce that evidence rather than silently extend its deadline.

## Validation boundary

Behavioral tests execute worker Bash and workflow wait/final-diagnostic steps with fake external services, real archives, and explicit failure/race fixtures. Renderer tests execute decoded payloads and reject failed decoding. Local tests do not establish remote image-build success. Require exact-head CI and independent review before one authorized full-preparation retry; no recurring cron or automatic repeat attempts.
