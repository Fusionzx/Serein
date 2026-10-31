# Task Breakdown

## Audit

- Existing Rust workspace with a pinned toolchain and synthetic API tests.
- Message sending already uses Discord's standard `POST /channels/{id}/messages` endpoint.
- REST currently applies a shared cooldown from ordinary rate-limit response headers; the Yapfast behavior worth adapting is bucket-aware admission based on Discord's response headers.
- The screenshot warning is produced independently by READY parsing when malformed account entries are skipped. The screenshot alone does not identify the affected payload or prove a send-path problem.
- Yapfast is GPL-3.0; Serein is MIT OR Apache-2.0. Implement the behavior independently and do not copy source.

## Task Breakdown

1. Add an internal route and bucket rate-limit coordinator in `crates/discord-api`.
   - Normalize routes while preserving Discord's major resource IDs.
   - Learn bucket IDs and remaining/reset values from response headers.
   - Honor route-specific and global 429 responses with bounded delays.
2. Integrate it in the shared request path.
   - Keep the REST endpoint and existing write safety rules.
   - Never retry a write after an ambiguous outcome.
3. Add focused synthetic regression coverage and update the compatibility note.
4. Review the READY warning separately; do not suppress it without diagnosing the skipped fields. No live account payload or credentials are used.

## Risk and scope

- Private normal-account Discord behavior is unofficial and remains live-unverified.
- Rate-limit responses and malformed READY payloads will be represented only by synthetic fixtures.
- No app binary replacement or release channel change is in scope for this local source patch.
