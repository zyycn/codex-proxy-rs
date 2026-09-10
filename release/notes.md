# v3.3.31

## What's new

- Add daily and weekly API key spending limits, persistent usage accounting, and audited reconciliation. Set a limit to `0` to leave it unrestricted.
- Support per-account outbound proxies for authentication, imports, and model requests.
- Support importing Codex personal access tokens.

## Improvements

- Configure scheduling, concurrency, weight, and groups before importing account credentials.
- Share account settings across import, individual editing, and batch editing, with consistent modal actions and spacing.
- Improve account plan labels and fill missing plan information from provider quota snapshots.

## Fixes

- Apply updated API key limits and enabled state to new requests on existing WebSocket connections.
- Prevent a flash at the end of light/dark theme transitions.
- Return explicit errors when a requested model is unavailable.

Spending limits use the existing dollar-based accounting rules. Requests already admitted can finish and cause settled usage to exceed a limit; usage-log cleanup does not reset budget accounting.
