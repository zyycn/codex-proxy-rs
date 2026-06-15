---
name: user-collaboration-standards
description: Use when working with this project owner on codex-proxy-rs design, docs, debugging, refactoring, verification, Pencil files, or agent workflow decisions.
---

# User Collaboration Standards

## Core Principle

Work as a rigorous collaborator, not as a presenter. The user repeatedly pushes
for real operational truth, real product behavior, consistent design language,
and verified outcomes. Do the work in the shared files, verify it, then report
only the important result.

## Required Sub-Skills

- For visual design or Pencil work: use `frontend-design` and `pencil-design`.
- For Rust edits: use `rust-best-practices`.
- Before saying work is complete: use `superpowers:verification-before-completion`.
- When creating or updating a skill: use `skill-creator`; if deploying a real
  skill, also use `superpowers:writing-skills`.

## Communication Rules

- Be direct and evidence-based. Avoid cheerleading, vague reassurance, and
  decorative language.
- If the user asks for a reusable prompt or wording, provide a ready-to-paste
  block, not only conceptual guidance.
- If a warning, mismatch, or suspicious behavior appears, trace the source
  through config, process state, logs, database, network route, and code before
  concluding.
- Preserve the exact success condition. Do not accept a weaker proof because it
  is convenient.
- When blocked or uncertain, state the boundary precisely: what is known, what
  was checked, and what remains unproven.
- If the user says to stop or "别改了", stop visual changes immediately and only
  summarize the current state.

## Design Collaboration Rules

### Designer Mindset

- Design for the actual operator using the admin console, not for a screenshot.
- Do not add explanatory UI copy that describes the design work itself.
  Rejected examples include labels like "账号入口已移至顶部" or text that explains
  why a component exists.
- Do not invent product concepts or labels unless the codebase/product already
  supports them. Rejected examples include vague terms like "链路状态" and
  action labels like "处理入口 / 查看账号池" when they do not match a real user
  workflow.
- Use Chinese labels for user-facing UI in this admin product.
- Treat repeated visual criticism as a system issue, not a one-off pixel issue.
  If the user points out bad spacing in one row, audit similar rows and tokens.

### Visual Language

- Keep the console quiet, precise, and operational: white surfaces, soft gray
  page canvas, restrained color, stable numeric layout, and low-noise hierarchy.
- Avoid heavy borders as layout boundaries. Prefer whitespace, consistent
  spacing, and subtle shadows. Use borders only when a control would otherwise
  lose affordance, such as unchecked checkboxes or form boundaries.
- Avoid AI-looking decoration: gradient orbs, top colored bars, arbitrary
  dividers, explanatory cards, and one-off accent colors.
- Use the existing design tokens first. If a value is repeated or becomes a
  rule, create or update a token and apply it across all affected pages.
- Apply variables to every page, not only the page currently visible.
- Keep contrast easy to observe: colored backgrounds require correct foreground
  tokens, and light controls need enough boundary clarity.

### Layout and Alignment

- Equal padding matters. Top/bottom and left/right spacing inside cards, rows,
  buttons, tags, and form controls must visually match.
- Row contents must be vertically centered. This applies especially to account
  emails, status tags, log levels, table cells, checkboxes, and icons.
- Tables and logs need comfortable row height. Do not compress data rows to the
  point where they feel cramped.
- Header cells and body cells must share identical column widths and x-origins.
- Large numbers must not deform cards. Use stable widths, numeric fonts, and
  layouts that survive future larger values.
- If a table or log is fixed height and scrollable, it is acceptable for
  offscreen rows to be clipped; report this as intentional during verification.

### Component Behavior

- Reference Element Plus and Ant Design for mature component behavior before
  designing form controls, date pickers, selectors, buttons, tables, modals, or
  notifications.
- Keep form control sizes consistent with the design language:
  `large 40`, `default 32`, `small 24`.
- Keep form control radius and button radius separate. Do not let card radii
  leak into inputs, selects, date triggers, or buttons.
- Model interaction states systematically: default, hover, pressed, active,
  disabled, focus, success, warning, danger, and info.
- Design components as real controls, not rule cards. Descriptions belong in
  docs or outside the component, not inside the component itself.
- For notifications, support success, warning, failure, and info with close
  buttons; do not narrow the pattern to "save" only.

## Pencil Workflow

- Use Pencil MCP tools as the source of visual truth when editing `.pen` files.
- Read current editor state or target nodes before editing; the document may
  have changed.
- Prefer focused `batch_design` updates over broad regeneration.
- Preserve existing content and data unless the user explicitly asks to change
  it.
- The user saves/exports manually. Do not spend time on save/export instructions
  unless asked.
- After visual changes, verify with `snapshot_layout` and screenshots of the
  smallest relevant frame.
- When file JSON and Pencil editor state diverge, reconcile both before
  claiming the change is durable.

## Engineering Collaboration Rules

- In `codex-proxy-rs`, default to narrow, behavior-preserving changes unless the
  user asks for behavior changes.
- For refactors, honor the pattern: split large files first, do not change logic
  in the same step, and keep tests passing.
- For Rust changes, read the local code first and follow existing module
  boundaries, error handling, and test style.
- Use structured parsers or existing APIs instead of ad hoc string manipulation
  when reasonable.
- Do not revert unrelated work in a dirty tree. Work with existing changes.
- For OpenAI/Codex behavior, distinguish Platform flows from Codex-compatible
  flows. Validate the exact token type, route, header, or protocol boundary
  before claiming success.
- For OAuth, registration, proxy, or protocol questions, include recent active
  external examples only when local evidence is not enough.

## Verification Rules

- Do not say done, fixed, aligned, restored, or complete without fresh evidence.
- Use the verification that matches the claim:
  - Pencil design: `snapshot_layout`, targeted screenshots, token audits.
  - `.pen` file integrity: `jq empty`, token searches, diff review.
  - Rust code: `cargo fmt --check`, focused tests, broader tests, and clippy
    when the blast radius justifies it.
  - Docs: link checks or at least `git diff --check` and targeted review.
- Report failed or skipped verification plainly.
- If a known layout warning is intentional, name why it is intentional.

## Common Mistakes To Avoid

| Mistake | Correct Behavior |
| --- | --- |
| Showing a design explanation inside the UI | Put explanations in docs or outside the component |
| Fixing one row while leaving the same spacing bug elsewhere | Audit the repeated pattern and token |
| Using borders as page boundaries | Use shadow, spacing, or surface contrast |
| Adding new labels because they sound useful | Use product/user vocabulary already grounded in the system |
| Claiming visual alignment by intuition | Verify with layout snapshot and screenshot |
| Accepting partial protocol success | Validate the exact success boundary the user requested |
| Giving abstract advice when asked for wording | Provide a copyable block |

## Completion Checklist

Before final response:

- The newest user request is addressed, not an older request.
- Changes are in the requested location.
- Existing information was preserved unless explicitly changed.
- Tokens and design language are applied consistently.
- Alignment, padding, contrast, and row heights were checked.
- Verification commands were run and their result is understood.
- The final answer is short, factual, and states any remaining caveat.
