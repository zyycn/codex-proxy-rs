# v3.3.32

## Fixes

- Remove manual charge reconciliation. Requests without available usage or pricing no longer block API keys; known costs from internal retries still count toward spending limits.
- Correct xAI subscription plan labels using current subscription information, without treating failed subscription lookups as Free accounts.
- Fix Codex custom tool calls, tool output pairing, and conversation continuations through xAI.
- Distinguish missing OpenAI WebSocket closing handshakes from TCP resets in connection diagnostics.

## Improvements

- Display daily and weekly API key amounts with two decimal places. Hover to see the full amount; accounting and limit checks retain their original precision.
- Include version-specific update notes on release pages.

## Upgrade

The database migration automatically settles legacy pending and unknown charges at zero and removes reconciliation state. Previously recorded costs and daily/weekly totals are preserved. Request errors and usage diagnostics remain available.
