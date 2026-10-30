import type { AccountQuotaWindow } from '@/api'
import { quotaWindowTone } from '@/components/account/account-usage-window/presenter'

// 与互联协议的账号上限一致，汇总数字不能触发无界节点分配
const MAX_POOL_NODES = 3_000

export interface PoolNode {
  id: string
  windows: AccountQuotaWindow[]
  tone: ReturnType<typeof quotaWindowTone>
}

export function poolNodes(windows: AccountQuotaWindow[], count?: number): PoolNode[] {
  const limit = count === undefined
    ? MAX_POOL_NODES
    : Math.min(MAX_POOL_NODES, Math.max(0, Math.trunc(count)))
  const accounts = new Map<string, AccountQuotaWindow[]>()
  for (const window of windows) {
    if (!window.sourceAccountId || window.amount)
      continue
    if (!accounts.has(window.sourceAccountId) && accounts.size >= limit)
      continue
    const group = accounts.get(window.sourceAccountId) ?? []
    group.push(window)
    accounts.set(window.sourceAccountId, group)
  }
  const nodes: PoolNode[] = [...accounts].map(([id, windows]) => {
    const percentages = windows.filter(window => !window.stale)
      .map(window => window.limitReached ? 100 : window.usedPercent)
      .filter((value): value is number => typeof value === 'number')
    return {
      id,
      windows,
      tone: quotaWindowTone(percentages.length
        ? Math.max(...percentages)
        : null),
    }
  })
  // 未公开逐账号额度时仅补灰色位置，不能把汇总可用数解释为各账号剩余额度
  if (count !== undefined) {
    while (nodes.length < limit)
      nodes.push({ id: `unobserved:${nodes.length}`, windows: [], tone: 'unknown' })
  }
  return nodes
}
