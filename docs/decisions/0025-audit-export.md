# 0025: Audit export as a singleton that keeps its place in the sink's status

Status: accepted for milestone 6

## Decision

- **An `AuditSink` is a cluster object** that names a destination: syslog over TCP, optionally with TLS and a server
  name, or an S3-compatible bucket with a region, prefix and keys. Only admins may read or change sinks.
- **One `audit-export` singleton** on a core member delivers every sink. It reads entries from the audit log in
  sequence, delivers up to 1,024 at a time, and records the next sequence in the sink's status only after the
  destination has accepted them. A new sink starts from the first entry.
- **Delivery is at least once and in order.** After a crash or a lease change the new holder resumes from the
  recorded sequence, so a destination can see a batch twice; every entry carries its sequence number to remove
  duplicates.
- **Syslog messages are RFC 5424 with octet counting** (RFC 6587), one message per entry with the entry as JSON.
  **Object storage receives one newline-delimited JSON object per delivery**, named
  `audit-<first>-<last>.ndjson` under the prefix and signed with AWS Signature Version 4.
- A failed delivery is recorded in `status.last_failure` and retried with backoff; the dashboard and
  `amafu audit-sink list` show it.

## Consequences

- The cluster keeps every audit entry, so a sink that falls behind catches up; its status shows how far it has got.
- Sink credentials live in the specification, readable by admins (0005).
