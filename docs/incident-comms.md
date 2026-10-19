# Incident communications

This document defines how the project communicates during a mainnet or
seed-fleet incident. It complements `docs/oncall-runbook.md` (technical
response) and `docs/mainnet-readiness.md` (launch gates).

## Channels

| Channel | Use | Owner |
| --- | --- | --- |
| Status page `<status-page-url>` | Canonical public status and updates | Launch lead |
| Telegram `<telegram-channel>` | Short public updates and links to the status page | Comms lead |
| X `<x-account>` | Launch and major incident announcements | Comms lead |
| ntfy topic `vtorrent-seeds-4254e0588837` | Internal pager alerts | On-call |
| GitHub issues | Technical follow-up and postmortem | Maintainers |

Replace the placeholders with the real URLs and handles before launch. Do not
publish a channel that is not monitored.

## Severity

| Level | Definition | Public update cadence |
| --- | --- | --- |
| SEV1 | Chain halt, consensus split, or credible risk to funds | Every 30 minutes until mitigated |
| SEV2 | Degraded availability, stalled staking, or one seed down | Every 2 hours |
| SEV3 | Minor degradation with no user impact | Daily or at resolution |

## Roles

- **Incident lead** owns the timeline, severity, and decisions.
- **Comms lead** writes and publishes updates. The incident lead approves
  SEV1 wording.
- **On-call engineer** runs the technical procedures in the on-call runbook.

One person may hold more than one role outside a SEV1.

## Rules

- State facts you have verified. Say "investigating" when the cause is unknown.
- Never speculate about attackers, fund loss, or recovery time.
- Never publish private keys, wallet files, passwords, or unredacted logs.
- Give the next update time in UTC. Keep it even when there is no new
  information.
- Link every short post to the status page entry.
- Correct errors explicitly in a new update. Do not silently edit a published
  incident.
- Keep internal technical detail in the incident channel, not in public posts.

## Templates

### Initial acknowledgment

```text
[Investigating] <service> issue
We are investigating reports of <symptom> affecting <scope>. No funds are
known to be at risk at this time. We will update by <UTC time>.
Status: <status-page-url>
```

### Update

```text
[Update] <service> issue
Current status: <investigating | identified | mitigating | monitoring>.
<Verified fact>. <User impact>. Next update by <UTC time>.
Status: <status-page-url>
```

### Resolved

```text
[Resolved] <service> issue
<service> is operating normally as of <UTC time>. <One-sentence cause>.
We will publish a post-incident summary within <N> business days.
Status: <status-page-url>
```

### Post-incident summary

```text
<Incident title>
Duration: <start> to <end> UTC
Severity: <SEV>
Impact: <who and what>
Cause: <verified cause>
Detection: <how it was found>
Resolution: <what restored service>
Follow-up: <issues and owners>
```

## Launch-day sequence

1. Publish a pre-launch status entry with the genesis hash and start time.
2. Publish a launch entry when the seeds agree on genesis and produce blocks.
3. If the launch aborts, publish the abort decision and the next attempt window.
4. Keep the status page updated until the first 24 hours are stable.

## Review

The incident lead reviews the timeline after every SEV1 or SEV2. File
follow-up issues and link them from the status page entry.
