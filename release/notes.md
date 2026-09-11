# v3.4.0

## What's new

- Add a dedicated proxy management page to create, edit, and test outbound proxies, with connection latency and exit IP information.
- Test new or edited proxy URLs before saving, without changing saved connection settings or linked accounts.
- Select managed proxies when importing accounts or editing individual and multiple accounts. Proxy changes apply to linked accounts without restarting the gateway.
- Search and paginate accounts linked to a proxy, with account identity, provider and authentication type, plan, and group information. Remove an account from its proxy to restore a direct connection.

## Fixes

- Apply the same certificate trust configuration to proxy tests and OpenAI requests, including `CODEX_CA_CERTIFICATE`, so usable proxies are not rejected by the selector because of inconsistent certificate handling.
- Protect credential imports from concurrent proxy changes, preserving credentials exchanged during import and their intended proxy binding.
- Correct the upstream WebSocket stream flag for non-streaming OpenAI client requests.
- Prevent table background seams after dragging dialogs.

## Improvements

- Keep proxy creation and editing consistent, with separate test and save actions and clearer input prompts. Display masked connection addresses in account selectors and preserve saved proxy credentials when the connection address is left unchanged.
- Refine proxy table column widths and compact actions, remove the redundant list refresh button, and reuse account plan labels across account management and linked-account dialogs.
- Make sidebar navigation scroll independently with an automatically hidden scrollbar, keeping the logo and footer controls accessible.
- Trim unnecessary trailing zeros from API key budget amounts and refine budget details and reset-time displays. Exact amounts remain available in the details popover.

## Upgrade

The database migration automatically adds existing account proxy URLs to the managed proxy catalog and preserves account bindings. Existing connections continue to work. Run a connection test before selecting a migrated proxy for a new or changed account binding.
