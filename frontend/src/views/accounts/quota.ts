import type { getAccounts } from '@/api'

import { clamp } from 'es-toolkit'

export type AccountRow = Awaited<ReturnType<typeof getAccounts>>['items'][number]
export type AccountQuotaWindow = AccountRow['quota']['windows'][number]

const summaryGroupOrder = new Map([
  ['shortTerm', 0],
  ['monthly', 1],
])

const panelGroupOrder = new Map([
  ['monthly', 0],
  ['shortTerm', 1],
  ['other', 2],
])

export function visibleSummaryQuotaWindows(windows: AccountQuotaWindow[]) {
  const known = [...windows]
    .filter(window => summaryGroupOrder.has(window.group))
    .sort((left, right) => groupOrder(left, summaryGroupOrder) - groupOrder(right, summaryGroupOrder))
  return known.length > 0 ? known : windows
}

export function orderedPanelQuotaWindows(windows: AccountQuotaWindow[]) {
  return [...windows].sort(
    (left, right) => groupOrder(left, panelGroupOrder) - groupOrder(right, panelGroupOrder),
  )
}

export function quotaWindowPercent(window: AccountQuotaWindow) {
  return clamp(window.usedPercent ?? 0, 0, 100)
}

export function quotaWindowBarStyle(window: AccountQuotaWindow, minimumWidth = '8px') {
  const percent = quotaWindowPercent(window)
  return {
    width: `${percent}%`,
    minWidth: percent > 0 ? minimumWidth : '0',
  }
}

export function quotaWindowBarClass(window: AccountQuotaWindow) {
  if (window.usedPercent === null)
    return 'bg-(--cp-default-border-hover)'
  if (window.usedPercent >= 95)
    return 'bg-(--cp-danger)'
  if (window.usedPercent >= 80)
    return 'bg-(--cp-warning)'
  return 'bg-(--cp-success)'
}

export function quotaWindowPercentTextClass(window: AccountQuotaWindow) {
  if (window.usedPercent === null)
    return 'text-(--cp-text-muted)'
  if (window.usedPercent >= 95)
    return 'text-(--cp-danger-text)'
  if (window.usedPercent >= 80)
    return 'text-(--cp-warning-text)'
  return 'text-(--cp-success-text)'
}

export function quotaWindowLocalUsageDisplay(window: AccountQuotaWindow) {
  return window.localUsage?.totalTokensDisplay || '-'
}

export function shouldShowQuotaWindowLocalUsage(window: AccountQuotaWindow) {
  return (window.localUsage?.totalTokens ?? 0) > 0
}

function groupOrder(window: AccountQuotaWindow, order: Map<string, number>) {
  return order.get(window.group) ?? order.size
}
