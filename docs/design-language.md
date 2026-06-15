# Codex Proxy RS Design Language

This document defines the visual language for the Codex Proxy RS admin console.
It is the source of truth for Pencil design iterations and future frontend
implementation.

## 1. Design Positioning

Codex Proxy RS is an operational console for account pools, routing, quota,
usage, service health, and logs. The interface should feel quiet, precise, and
technical, closer to Codex/OpenAI tooling than to a colorful SaaS dashboard.

The design language is:

- restrained: white surfaces, soft gray canvas, minimal decoration;
- operational: dense enough for repeated daily use, but not cramped;
- numeric: important values use monospace and stable widths;
- state-driven: color is reserved for status, risk, and selection;
- modern: hierarchy comes from spacing, typography, and subtle shadow, not heavy
  borders.

Avoid marketing-page composition, decorative gradients, loud cards, nested card
stacks, heavy borders, and arbitrary accent colors.

## 2. Color System

Use a neutral-first palette. Accent colors must communicate state or selection.
Do not create new blues, greens, grays, or reds unless the token system is
updated.

### Core Tokens

| Token | Value | Usage |
| --- | --- | --- |
| `--cp-bg-page` | `#F6F8FB` | Main app canvas |
| `--cp-bg-surface` | `#FFFFFF` | Sidebar, cards, popovers |
| `--cp-bg-subtle` | `#F8FAFC` | Table rows, inner status areas |
| `--cp-bg-muted` | `#F1F5F9` | Segmented controls, neutral badges |
| `--cp-bg-nav-active` | `#E9EEF5` | Selected sidebar navigation row |
| `--cp-text-primary` | `#0E1726` | Main text and headings |
| `--cp-text-strong` | `#111827` | Brand mark and high emphasis |
| `--cp-text-secondary` | `#64748B` | Descriptions, labels |
| `--cp-text-muted` | `#94A3B8` | Table headers, low-emphasis labels |
| `--cp-white` | `#FFFFFF` | Inverse text |
| `--cp-transparent` | `#FFFFFF00` | Transparent Pencil fills and inactive containers |
| `--cp-overlay-scrim` | `#0E17264D` | Modal and blocking interaction overlay |

### Semantic Color Model

The state system follows the same structure as mature component libraries:
Element Plus exposes `base`, `light-*`, and `dark-*` levels per semantic color;
Ant Design derives alias tokens such as `colorSuccessBg`,
`colorSuccessBgHover`, `colorSuccessBorder`, `colorSuccessHover`, and
`colorSuccessActive` from seed colors. Codex Proxy RS uses the same token shape,
but keeps its own quieter operational palette.

Use `pressed` for the momentary mouse-down state. Use `active` for a persistent
selected/current state. Do not use `active` to mean hover.

### Neutral Interaction Tokens

| Token | Value | Usage |
| --- | --- | --- |
| `--cp-default-bg` | `#F8FAFC` | Default control background |
| `--cp-default-bg-hover` | `#F1F5F9` | Default control hover |
| `--cp-default-bg-active` | `#E9EEF5` | Selected neutral row or nav item |
| `--cp-default-border` | `#E2E8F0` | Subtle control boundary when needed |
| `--cp-default-border-hover` | `#CBD5E1` | Hover boundary when needed |
| `--cp-default-text` | `#0E1726` | Default control text |
| `--cp-default-text-hover` | `#111827` | Hover text |
| `--cp-default-text-active` | `#0B1220` | Active text |
| `--cp-disabled-bg` | `#F1F5F9` | Disabled control background |
| `--cp-disabled-border` | `#E2E8F0` | Disabled boundary |
| `--cp-disabled-text` | `#94A3B8` | Disabled text |
| `--cp-disabled-icon` | `#CBD5E1` | Disabled icon |

### Semantic State Tokens

Each semantic state has the same token shape. `bg` is for quiet badges,
filled rows, selected light controls, and notification icon wells. `solid` is
reserved for rare high-emphasis controls and charts.

| State | Base | Hover | Pressed | Background | Bg Hover | Active Bg | Border | Border Hover | Text |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `info` | `#2563EB` | `#1D4ED8` | `#1E40AF` | `#EEF6FF` | `#DBEAFE` | `#BFDBFE` | `#93C5FD` | `#60A5FA` | `#2563EB` |
| `normal` | `#0F9F9A` | `#0D8D88` | `#0B7370` | `#ECFDFD` | `#DDF7F6` | `#C7EFEE` | `#A7DFDD` | `#6BC6C2` | `#0B7370` |
| `success` | `#12B981` | `#0EA371` | `#0B865F` | `#ECFDF5` | `#DDFBEA` | `#C8F5DC` | `#B7EAD0` | `#82D9B3` | `#047857` |
| `warning` | `#F59E0B` | `#D97706` | `#B45309` | `#FFFBEB` | `#FEF3C7` | `#FDE68A` | `#FCD34D` | `#FBBF24` | `#B45309` |
| `danger` | `#EF4444` | `#DC2626` | `#B91C1C` | `#FEF2F2` | `#FEE2E2` | `#FECACA` | `#FCA5A5` | `#F87171` | `#B91C1C` |

### Solid Foreground Tokens

Do not assume every colored solid background uses white text. Foreground tokens
are selected by contrast against the actual background. Normal text should target
at least `4.5:1` contrast; small data labels should use the stronger foreground
when there is any doubt.

| State | Base Foreground | Hover Foreground | Pressed Foreground |
| --- | --- | --- | --- |
| `info` | `--cp-info-on` `#FFFFFF` | `--cp-info-hover-on` `#FFFFFF` | `--cp-info-pressed-on` `#FFFFFF` |
| `normal` | `--cp-normal-on` `#0B1220` | `--cp-normal-hover-on` `#0B1220` | `--cp-normal-pressed-on` `#FFFFFF` |
| `success` | `--cp-success-on` `#0B1220` | `--cp-success-hover-on` `#0B1220` | `--cp-success-pressed-on` `#FFFFFF` |
| `warning` | `--cp-warning-on` `#0B1220` | `--cp-warning-hover-on` `#0B1220` | `--cp-warning-pressed-on` `#FFFFFF` |
| `danger` | `--cp-danger-on` `#0B1220` | `--cp-danger-hover-on` `#FFFFFF` | `--cp-danger-pressed-on` `#FFFFFF` |

### Compatibility Aliases

The older tint token names remain as aliases during design iteration:

| Alias | Use |
| --- | --- |
| `--cp-accent` | Same as `--cp-info` |
| `--cp-accent-tint` | Same as `--cp-info-bg` |
| `--cp-info-tint` | Same as `--cp-info-bg` |
| `--cp-normal-tint` | Same as `--cp-normal-bg` |
| `--cp-success-tint` | Same as `--cp-success-bg` |
| `--cp-warning-tint` | Same as `--cp-warning-bg` |
| `--cp-danger-tint` | Same as `--cp-danger-bg` |
| `--cp-teal` | Same as `--cp-normal` |
| `--cp-teal-tint` | Same as `--cp-normal-bg` |

Legacy Pencil-only names such as `primary-blue`, `purple`, `success-green`,
`warning-orange`, `error-red`, `cyan`, `info`, `normal`, `bg-card`,
`bg-sidebar`, `text-primary`, `text-secondary`, and `text-muted` are not part of
the design language. Do not use them in new components; migrate existing work to
`cp-*` tokens when touching the affected layer.

### Color Rules

- Use `--cp-bg-nav-active` with `--cp-text-strong` for selected sidebar
  navigation.
- Use `--cp-info` for selected tabs, focused actions, and primary data lines.
- Use state colors only when the value is actually stateful.
- Prefer semantic `bg` tokens for component states. Use `solid` state colors
  only when a surface must visually dominate.
- When using `base`, `hover`, or `pressed` as a solid background, use the
  matching `on` foreground token. Never place white text on warning/success
  solids by default.
- Hover uses the `hover` or `bg-hover` token. Mouse-down uses `pressed`.
  Persistent selection uses `bg-active` plus state text.
- Default component interactions should stay in the light token layer:
  default uses neutral bg, hover uses semantic bg, pressed uses semantic
  `bg-active`, and selected uses neutral active bg plus a small semantic
  indicator. Do not jump from a light default control directly into a solid
  saturated pressed state unless the component is already a solid primary
  action.
- Operational status semantics:
  - `正常`: `--cp-normal`;
  - `成功`: `--cp-success`;
  - `警告`: `--cp-warning`;
  - `错误`: `--cp-danger`.
- Feedback toast semantics:
  - `成功`: `--cp-success`;
  - `警告`: `--cp-warning`;
  - `失败`: `--cp-danger`;
  - `信息`: `--cp-info`.
- Do not assign a unique color to every card.
- Prefer text plus small state color over large filled color blocks.
- Keep charts limited to `accent`, `success`, `warning`, and `danger`.

## 3. Typography

Use two font roles.

| Role | Font | Usage |
| --- | --- | --- |
| Interface | `Inter` | Navigation, labels, headings, controls |
| Numeric/data | `JetBrains Mono` | Metrics, IDs, timestamps, status codes, latency |

### Type Scale

| Token | Size | Line Height | Weight | Usage |
| --- | ---: | ---: | ---: | --- |
| `--cp-text-display` | `34` | `1.15` | `760-800` | Page title only |
| `--cp-text-section` | `20` | `1.15` | `720-760` | Card titles |
| `--cp-text-brand` | `17` | `1.1` | `700-720` | Sidebar brand |
| `--cp-text-body` | `14` | `1.2` | `600-650` | Navigation and body text |
| `--cp-text-label` | `13` | `1.15` | `600-700` | Labels and row names |
| `--cp-text-caption` | `12` | `1.15` | `600` | Metadata and helper text |
| `--cp-text-micro` | `11` | `1.15` | `600` | Dense badges and table hints |
| `--cp-number-lg` | `28` | `1.05` | `760-800` | Top metric values |
| `--cp-number-md` | `24` | `1.1` | `760` | Secondary numeric summaries |
| `--cp-number-sm` | `12` | `1.15` | `650-720` | Table and compact numeric values |

### Typography Rules

- Avoid using `800+` weight except for page title or critical numeric values.
- Labels should not exceed `700`.
- Use `JetBrains Mono` for values that must visually align or compare.
- Do not use negative letter spacing.
- Do not scale font size with viewport width.

## 4. Spacing System

Use a 4px base grid, with `24px` as the primary dashboard rhythm.

| Token | Value | Usage |
| --- | ---: | --- |
| `--cp-space-1` | `4` | Micro gaps |
| `--cp-space-2` | `8` | Icon/text gaps |
| `--cp-space-3` | `12` | Compact padding |
| `--cp-space-4` | `16` | Control padding |
| `--cp-space-5` | `20` | Dense card internals |
| `--cp-space-6` | `24` | Main card gaps and standard card padding |
| `--cp-space-7` | `28` | Large card internal padding |
| `--cp-space-8` | `32` | Header groups |

### Layout Rhythm

- Main card gaps: `24px` horizontally and vertically.
- Large card internal padding: `28px` when content is complex.
- Top metric internal padding: `24px`.
- Table row height: `52-56px`, never below `48px`.
- Popover item height: `36-40px`.
- Sidebar navigation row height: `46px`.

## 5. Radius System

Use separate radius groups for controls and surfaces. Do not let card, nav, or
pill radii leak into form controls.

| Token | Value | Usage |
| --- | ---: | --- |
| `--cp-border-radius-base` | `4` | Inputs, selects, date/time triggers |
| `--cp-button-radius-base` | `6` | Large/default buttons |
| `--cp-button-radius-small` | `4` | Small buttons |
| `--cp-border-radius-small` | `2` | Small square controls |
| `--cp-radius-sm` | `10` | Icon backgrounds, table rows |
| `--cp-radius-md` | `12` | Nav items, segmented controls |
| `--cp-radius-lg` | `16` | Metric cards |
| `--cp-radius-xl` | `18` | Large dashboard cards, popovers |
| `--cp-radius-xxl` | `22` | Component spec panels and demo canvases |
| `--cp-checkbox-radius` | `5` | Checkbox squares |
| `--cp-tag-radius` | `7` | Status tags and removable selected values |
| `--cp-date-cell-radius` | `8` | Date picker cells |
| `--cp-icon-button-radius` | `8` | Compact square icon buttons |
| `--cp-radius-pill` | `999` | Status pills only |

Rules:

- Buttons never use card, nav, or pill radius tokens.
- Main cards: `16-18px`.
- Component specification panels may use `22px` because they act as demo
  canvases, not product dashboard cards.
- Nav rows: `10-12px`. Form buttons use a softer Codex Proxy RS override:
  base `6px`, small `4px`.
- Tags, checkboxes, date cells, and compact icon buttons use their dedicated
  radius tokens instead of borrowing card or button radii.
- Status pills: `999px`.
- Avoid using pill radius on large rectangular UI.

## 6. Shadow System

Shadows create layer hierarchy. They should not look like borders or glow.

| Token | Value | Usage |
| --- | --- | --- |
| `--cp-shadow-sidebar` | `2px 0 12px -12px #0E172607` | Sidebar separation |
| `--cp-shadow-card` | `0 10px 22px -18px #0E172614` | Standard cards |
| `--cp-shadow-control` | `0 8px 18px -16px #0E172614` | Header controls, selected tab |
| `--cp-shadow-popover` | `0 16px 34px -18px #0E172622` | Dropdown and floating menus |

Pencil stores shadow geometry and shadow color separately. Use these color
variables when applying shadow effects in `.pen` files:

| Token | Value | Usage |
| --- | --- | --- |
| `--cp-shadow-sidebar-color` | `#0E172607` | Sidebar shadow color |
| `--cp-shadow-card-color` | `#0E172614` | Card and control shadow color |
| `--cp-shadow-popover-color` | `#0E172622` | Popover and toast shadow color |

Rules:

- Never use borders as primary page separation.
- Card shadows must be weaker than popover shadows.
- Sidebar shadow must be barely visible.
- Avoid stacking multiple heavy shadows in the same local area.

## 7. Element Plus Token Alignment

Use Element Plus as the behavioral reference for base component variables, while
keeping Codex Proxy RS colors and shadow language.

| Element Plus token | Codex Proxy RS token | Value |
| --- | --- | --- |
| `--el-component-size-large` | `--cp-component-size-large` | `40px` |
| `--el-component-size` | `--cp-component-size-default` | `32px` |
| `--el-component-size-small` | `--cp-component-size-small` | `24px` |
| `--el-border-radius-base` | `--cp-border-radius-base` | `4px` |
| `--el-border-radius-small` | `--cp-border-radius-small` | `2px` |
| `--el-border-radius-round` | `--cp-border-radius-round` | `20px` |
| Element Plus button radius + product override | `--cp-button-radius-base` | `6px` |
| Element Plus small button radius + product override | `--cp-button-radius-small` | `4px` |
| `--el-input-padding-horizontal-large` | `--cp-input-padding-x-large` | `16px` |
| `--el-input-padding-horizontal` | `--cp-input-padding-x-default` | `12px` |
| `--el-input-padding-horizontal-small` | `--cp-input-padding-x-small` | `8px` |
| `--el-select-option-height` | `--cp-select-option-height` | `34px` |
| `--el-date-editor-width` | `--cp-date-editor-width` | `220px` |
| `--el-date-editor-daterange-width` | `--cp-date-editor-daterange-width` | `350px` |
| `--el-date-editor-datetimerange-width` | `--cp-date-editor-datetimerange-width` | `400px` |
| `--el-popper-border-radius` | `--cp-popper-radius` | `4px` |

Rules:

- Input, select, date, time, button, and similar form triggers share the same
  component size tokens.
- Do not create one-off heights such as `34px`, `36px`, or `46px` for normal
  form triggers. `34px` is reserved for select option rows and picker cells.
- Do not create one-off rounded control radii such as `9px`, `10px`, or `12px`.
  Use the Element Plus radius model above.
- Buttons intentionally use a slightly softer product override: `6px` for
  large/default and `4px` for small. Other form controls stay on the Element
  Plus base radius.

## 8. Icon System

Use Lucide icons.

| Context | Size | Color |
| --- | ---: | --- |
| Sidebar nav | `20` | active `text-strong`, inactive `text-secondary` |
| Card icon | `18` | state token |
| Service row icon | `14` | state token |
| Toolbar icon | `16-19` | semantic token |
| Brand icon | `18` | white on dark surface |
| Notification icon | `18-19` | `text-secondary`, badge uses state token |

Rules:

- Icons support recognition; they do not replace essential text in expanded
  views.
- Collapsed sidebar may show icons only.
- Sidebar nav icons and labels must share the same vertical center.
- Prefer simple line icons. Avoid custom decorative icon compositions unless
  they become a formal brand asset.

## 9. Layout System

### Canvas

- Desktop design target: `1920px` wide.
- Height may grow.
- Sidebar states:
  - collapsed: `88px`;
  - expanded: `280px`.

### Main Content

- Main content width should be stable across sidebar states.
- Main content outer gutters should feel balanced after accounting for sidebar.
- Main card grid gap: `24px`.
- Prefer two primary columns before three.
- Use full-width rows for high-complexity data sections.

### Sidebar

- Sidebar background: `--cp-bg-surface`.
- Separation: shadow, not visible border.
- Brand area should be compact and quiet.
- Collapse control sits near bottom, not in the brand area.
- Nav starts high enough that the top area does not feel empty.

## 10. Component Rules

### Form Controls

- Basic form controls follow the Element Plus size model while retaining
  Codex Proxy RS colors and shadows:
  - `large`: `40px`;
  - `default`: `32px`;
  - `small`: `24px`.
- These values map to Element Plus `--el-component-size-large`,
  `--el-component-size`, and `--el-component-size-small`.
- Form control radius follows Element Plus `--el-border-radius-base`: `4px`.
  Small square controls use `--el-border-radius-small`: `2px`. Buttons use
  `--cp-button-radius-base` and `--cp-button-radius-small`.
- Input horizontal padding follows Element Plus input padding:
  `large 16px`, `default 12px`, `small 8px`.
- Default input surface uses `--cp-bg-subtle`; do not depend on heavy borders.
- Focused input uses `--cp-bg-surface` plus `--cp-shadow-control`.
- Error input uses `--cp-danger-tint`, `--cp-danger`, and a short helper row.
- Disabled input uses `--cp-bg-muted` and `--cp-text-muted`.
- Labels use `--cp-text-caption` scale and `--cp-text-secondary`.
- Prefix icons, suffix icons, clear controls, and validation icons must be
  vertically centered with the input value.
- Time, IDs, keys, and stable technical values use `JetBrains Mono`.

### Selectors and Time Pickers

- Basic selector trigger sizes follow `large` `40px`, `default` `32px`, and
  `small` `24px`; these share the same control height tokens as input and
  button.
- Date and time picker triggers are date-editor inputs and therefore also use
  the same `40 / 32 / 24px` component size tokens. Do not create a separate
  `46px` date trigger height.
- Trigger radius: `4px`.
- Dropdown and picker surfaces use `--cp-bg-surface` and
  `--cp-shadow-popover`; do not add borders.
- Dropdown radius follows Element Plus popper radius: `4px`.
- Select option row height follows Element Plus `--el-select-option-height`:
  `34px`.
- Selected menu rows use `--cp-info-bg` with `--cp-info-text`; hover rows use
  `--cp-default-bg-hover`; disabled rows use `--cp-disabled-text`.
- Multi-select values use compact tags inside the trigger, not separate chips
  outside the component. Selected tags must include a close `x` affordance when
  the value can be removed.
- Time picker values use `JetBrains Mono`.

### Date Pickers

- Required variants: `单日期`, `日期区间`, and `单时间`.
- Trigger height uses the shared form control size tokens, defaulting to `32px`;
  trigger radius uses `4px`.
- Date editor widths follow Element Plus defaults unless the surrounding product
  layout requires a wider field: single date/time `220px`, date range `350px`,
  datetime range `400px`.
- Date and time picker popovers use `--cp-bg-surface` or
  `--cp-bg-subtle` with `--cp-shadow-popover`; avoid visible borders.
- Single date follows the same Element Plus style interaction model as the
  range picker: the trigger only displays the current value and clear affordance;
  shortcuts live inside the popover sidebar when present; month navigation,
  date grid, selected value, and apply actions live inside the popover.
- Date range uses one trigger field, not separate start and end inputs. The
  displayed value format is `YYYY-MM-DD - YYYY-MM-DD`.
- Date range follows an Element Plus style interaction model: the trigger only
  displays the current value; shortcuts live inside the popover sidebar; the
  main popover body shows two adjacent month panels; the footer shows the
  current range plus clear, cancel, and confirm actions.
- Range month panels show a stable 7-column by 6-row grid so keyboard movement,
  hover preview, and disabled days do not change layout height.
- Date cells are `44-48px` wide and `32-38px` high.
- Date cell text uses `JetBrains Mono` for stable numeric alignment.
- Default dates use transparent fill and `--cp-text-primary`.
- Hover dates use `--cp-default-bg-hover` and `--cp-text-strong`.
- Today uses `--cp-info-bg` with `--cp-info-text`.
- Selected dates use `--cp-info` with `--cp-info-on`.
- Disabled or outside-month dates use `--cp-disabled-bg` or transparent fill
  with `--cp-disabled-text`; they should never look selectable.
- Range endpoints use the selected style. Range middle dates use
  `--cp-info-bg` with `--cp-info-text` so the interval reads as continuous
  without heavy outlines.
- Single-time pickers use fixed columns for hour, minute, and second. Time rows
  are `34-36px` high, use `JetBrains Mono`, and must not be clipped by the
  popover. The footer actions belong inside the popover, not next to the
  trigger.
- Date component state previews should show real trigger and date-cell variants:
  default, focus, disabled, error, hover, today, selected, and disabled date
  cells. Do not replace state previews with explanatory rule cards.
- Picker action buttons stay quiet: secondary actions use `--cp-bg-subtle`;
  primary apply actions use `--cp-info` with `--cp-info-on`.
- Picker footer action buttons use the default button height of `32px`.
  `34px` is reserved for picker option rows or date cells, not normal buttons.

### Buttons and Checkboxes

- Buttons follow the Element Plus size model:
  - `large`: `40px` height;
  - `default`: `32px` height;
  - `small`: `24px` height.
- Do not create `34px` default buttons. If a control is visually button-like
  and triggers an action, it must use one of the three button sizes above.
- Button radius: `6px` for large/default, `4px` for small.
- Button icon size: `14-16px`; small buttons use `12px` icons.
- Primary actions use `--cp-info` with `--cp-info-on` when they are the main
  apply action. Quiet primary-like actions can use `--cp-info-bg` with
  `--cp-info-text`.
- Reserve solid accent or dark filled buttons for a single screen-level primary
  action only; do not use multiple heavy filled buttons inside component groups.
- Secondary actions use `--cp-bg-subtle`.
- Destructive actions use `--cp-danger-tint` and `--cp-danger`.
- Disabled actions use `--cp-bg-muted`, `--cp-text-muted`, and reduced
  opacity.
- Button states must be represented as normal, hover, loading, disabled, and
  text-button variants when building component specs.
- Checkbox size: `18px`; radius: `5px`.
- Unchecked checkbox squares use `--cp-bg-surface` with
  `--cp-default-border-hover` as a light inner boundary on subtle surfaces.
- Checked checkbox rows may use `--cp-info-tint`; the checkbox square itself
  stays light and the check icon uses `--cp-accent`.
- Mixed checkbox squares use `--cp-info-tint` with an `--cp-accent` minus icon.
- Avoid solid accent-filled checkbox squares inside light component groups.
- Checkbox label and box must share a vertical center.

### Brand Area

- Collapsed: only the brand icon.
- Expanded:
  - icon: dark square terminal mark;
  - primary name: `Codex`;
  - secondary text: `Proxy RS · version`.
- Brand should not use a badge that looks like an action.

### Metric Cards

- Use neutral white surfaces.
- The primary value is the visual anchor.
- Detail band uses subtle background, not divider lines.
- Metric values must support large numbers without changing layout.
- Do not use top colored bars.

### Large Cards

- Title at top-left, action controls at top-right.
- Card padding should be visually equal on all sides.
- Do not put card sections inside card-looking nested cards unless the inner
  surface is functional.

### Tables and Logs

- Row height: `52-56px`.
- Header height: `40-44px`.
- Use `JetBrains Mono` for time, IDs, codes, and latency.
- Tables follow Element Plus structure: optional selection column, stable
  header row, striped body rows, semantic status tags, right-aligned operation
  text buttons, and pagination at the footer when row count exceeds the visible
  page.
- Table header cells and body cells must share identical column widths and the
  same x-origin. Do not position body rows independently from the header.
- Fixed-height log viewport may clip offscreen rows to imply scroll.
- Level labels should be Chinese in the UI.

### Popovers

- Width should match content, not the trigger width.
- Use `--cp-shadow-popover`.
- Do not add borders.
- Menu rows use `36-40px` height.
- Destructive actions use danger text and icon only.

### Modal Dialogs

- Use modals for creation, confirmation, destructive confirmation, and
  medium-complexity forms that should not navigate away from the current page.
- Modal overlay uses `--cp-overlay-scrim`.
- Modal surface uses `--cp-bg-surface`, `--cp-shadow-popover`, and no visible
  border.
- Default widths: `480px` for confirmation, `560px` for standard forms,
  `720px` for dense forms.
- Internal padding: `28px`; header icon well: `36-44px`; close button: `28px`.
- Footer actions are right-aligned. The primary action is visually strongest;
  destructive actions use danger tint and danger text rather than a large red
  solid block by default.
- Modal semantic variants are `信息`, `警告`, `失败`, and `成功`. Variant examples
  should keep the same compact modal anatomy: icon well, title, one short
  description, close button, and footer actions only.
- Do not add tip rows, form rows, or unrelated scenario content inside modal
  variant examples. If explanation is needed, place it outside the component.
- Modal footer buttons use the default button height of `32px`.

### Feedback Toast

- Feedback toast appears as a floating message after a user or system operation.
- It is not a persistent top-bar notification entry.
- Width: `320-360px`; height: `52-64px`.
- Position: top-right of the active workspace, offset by `24px`.
- Use `--cp-shadow-popover`; do not add borders.
- Icon background uses the matching tint token.
- Toast text is concise:
  - success: `成功`;
  - warning: `警告`;
  - failure: `失败`;
  - info: `信息`.
- Use `--cp-success`, `--cp-warning`, `--cp-danger`, and `--cp-info` for the
  leading icon and optional state accent.
- Feedback toasts do not include inline actions.
- Every toast includes a right-aligned close button.
- Close button: `28px`, transparent background, `x` icon at `16px`,
  `--cp-text-muted`; it stays vertically centered with the icon and text.

### Status Rows

- Use subtle row backgrounds.
- Icon, label, value, and metadata should share a vertical center.
- State color belongs on icon/value, not the entire row.

## 11. Copy Rules

- Use user-facing names, not internal implementation terms.
- Avoid explanatory UI copy such as "moved to top" or "dropdown opened".
- Labels should describe what the user recognizes: `账号`, `请求`, `Token`,
  `平均响应`, `服务状态`, `事件日志`.
- Avoid vague system phrases like `链路状态` unless the product has a concrete
  user-facing definition.

## 12. Design Quality Checklist

Before accepting a new screen or iteration:

- Main gutters are balanced for the current sidebar state.
- Main card gaps are consistently `24px`.
- Top, bottom, left, and right padding inside each card are visually equal.
- Text is vertically centered inside rows, badges, and buttons.
- Text on colored backgrounds uses the matching `on` foreground token and is
  readable at small sizes.
- No major layout boundary depends on a visible border.
- Colors come from this document.
- Font weights do not exceed the defined role.
- Tables have comfortable row height.
- Large numbers fit without layout shift.
- Popovers appear above cards with stronger hierarchy than cards.
- Both collapsed and expanded sidebar states are checked.

## 13. Implementation Token Draft

```css
:root {
  --cp-bg-page: #F6F8FB;
  --cp-bg-surface: #FFFFFF;
  --cp-bg-subtle: #F8FAFC;
  --cp-bg-muted: #F1F5F9;
  --cp-bg-nav-active: #E9EEF5;

  --cp-text-primary: #0E1726;
  --cp-text-strong: #111827;
  --cp-text-secondary: #64748B;
  --cp-text-muted: #94A3B8;
  --cp-white: #FFFFFF;
  --cp-transparent: #FFFFFF00;
  --cp-overlay-scrim: #0E17264D;

  --cp-accent: #2563EB;

  --cp-component-size-large: 40px;
  --cp-component-size-default: 32px;
  --cp-component-size-small: 24px;
  --cp-border-radius-base: 4px;
  --cp-border-radius-small: 2px;
  --cp-button-radius-base: 6px;
  --cp-button-radius-small: 4px;
  --cp-border-radius-round: 20px;
  --cp-input-padding-x-large: 16px;
  --cp-input-padding-x-default: 12px;
  --cp-input-padding-x-small: 8px;
  --cp-select-option-height: 34px;
  --cp-select-item-height: 24px;
  --cp-date-editor-width: 220px;
  --cp-date-editor-daterange-width: 350px;
  --cp-date-editor-datetimerange-width: 400px;
  --cp-popper-radius: 4px;

  --cp-default-bg: #F8FAFC;
  --cp-default-bg-hover: #F1F5F9;
  --cp-default-bg-active: #E9EEF5;
  --cp-default-border: #E2E8F0;
  --cp-default-border-hover: #CBD5E1;
  --cp-default-text: #0E1726;
  --cp-default-text-hover: #111827;
  --cp-default-text-active: #0B1220;

  --cp-disabled-bg: #F1F5F9;
  --cp-disabled-border: #E2E8F0;
  --cp-disabled-text: #94A3B8;
  --cp-disabled-icon: #CBD5E1;

  --cp-info: #2563EB;
  --cp-info-hover: #1D4ED8;
  --cp-info-pressed: #1E40AF;
  --cp-info-on: #FFFFFF;
  --cp-info-hover-on: #FFFFFF;
  --cp-info-pressed-on: #FFFFFF;
  --cp-info-bg: #EEF6FF;
  --cp-info-bg-hover: #DBEAFE;
  --cp-info-bg-active: #BFDBFE;
  --cp-info-border: #93C5FD;
  --cp-info-border-hover: #60A5FA;
  --cp-info-text: #2563EB;

  --cp-success: #12B981;
  --cp-success-hover: #0EA371;
  --cp-success-pressed: #0B865F;
  --cp-success-on: #0B1220;
  --cp-success-hover-on: #0B1220;
  --cp-success-pressed-on: #FFFFFF;
  --cp-success-bg: #ECFDF5;
  --cp-success-bg-hover: #DDFBEA;
  --cp-success-bg-active: #C8F5DC;
  --cp-success-border: #B7EAD0;
  --cp-success-border-hover: #82D9B3;
  --cp-success-text: #047857;

  --cp-normal: #0F9F9A;
  --cp-normal-hover: #0D8D88;
  --cp-normal-pressed: #0B7370;
  --cp-normal-on: #0B1220;
  --cp-normal-hover-on: #0B1220;
  --cp-normal-pressed-on: #FFFFFF;
  --cp-normal-bg: #ECFDFD;
  --cp-normal-bg-hover: #DDF7F6;
  --cp-normal-bg-active: #C7EFEE;
  --cp-normal-border: #A7DFDD;
  --cp-normal-border-hover: #6BC6C2;
  --cp-normal-text: #0B7370;

  --cp-warning: #F59E0B;
  --cp-warning-hover: #D97706;
  --cp-warning-pressed: #B45309;
  --cp-warning-on: #0B1220;
  --cp-warning-hover-on: #0B1220;
  --cp-warning-pressed-on: #FFFFFF;
  --cp-warning-bg: #FFFBEB;
  --cp-warning-bg-hover: #FEF3C7;
  --cp-warning-bg-active: #FDE68A;
  --cp-warning-border: #FCD34D;
  --cp-warning-border-hover: #FBBF24;
  --cp-warning-text: #B45309;

  --cp-danger: #EF4444;
  --cp-danger-hover: #DC2626;
  --cp-danger-pressed: #B91C1C;
  --cp-danger-on: #0B1220;
  --cp-danger-hover-on: #FFFFFF;
  --cp-danger-pressed-on: #FFFFFF;
  --cp-danger-bg: #FEF2F2;
  --cp-danger-bg-hover: #FEE2E2;
  --cp-danger-bg-active: #FECACA;
  --cp-danger-border: #FCA5A5;
  --cp-danger-border-hover: #F87171;
  --cp-danger-text: #B91C1C;

  --cp-teal: #0F9F9A;

  --cp-accent-tint: #EEF6FF;
  --cp-info-tint: #EEF6FF;
  --cp-success-tint: #ECFDF5;
  --cp-normal-tint: #ECFDFD;
  --cp-warning-tint: #FFFBEB;
  --cp-danger-tint: #FEF2F2;
  --cp-teal-tint: #ECFDFD;

  --cp-space-1: 4px;
  --cp-space-2: 8px;
  --cp-space-3: 12px;
  --cp-space-4: 16px;
  --cp-space-5: 20px;
  --cp-space-6: 24px;
  --cp-space-7: 28px;
  --cp-space-8: 32px;

  --cp-radius-sm: 10px;
  --cp-radius-md: 12px;
  --cp-radius-lg: 16px;
  --cp-radius-xl: 18px;
  --cp-radius-xxl: 22px;
  --cp-checkbox-radius: 5px;
  --cp-tag-radius: 7px;
  --cp-date-cell-radius: 8px;
  --cp-icon-button-radius: 8px;
  --cp-radius-pill: 999px;

  --cp-shadow-sidebar: 2px 0 12px -12px #0E172607;
  --cp-shadow-card: 0 10px 22px -18px #0E172614;
  --cp-shadow-control: 0 8px 18px -16px #0E172614;
  --cp-shadow-popover: 0 16px 34px -18px #0E172622;
  --cp-shadow-sidebar-color: #0E172607;
  --cp-shadow-card-color: #0E172614;
  --cp-shadow-popover-color: #0E172622;
}
```
