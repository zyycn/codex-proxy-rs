import type { AccountQuotaWindow, AccountQuotaWindowEntry, AccountRow } from '../../constants'

type AccountModelUsage = AccountRow['usage']['models'][number]

/**
 * 根据账号最近一次模型请求选择对应额度组。
 * 没有专属额度的模型统一回退到 Codex 通用额度。
 */
export function recentlyUsedQuotaEntry(
  entries: readonly AccountQuotaWindowEntry[],
  models: readonly AccountModelUsage[],
) {
  const recentModel = latestModelUsage(models)
  if (recentModel) {
    const modelKey = quotaIdentity(recentModel.model)
    const modelEntry = entries.find(entry => quotaEntryIdentities(entry).has(modelKey))
    if (modelEntry)
      return modelEntry
  }

  return entries.find(entry => entry.windows.some(window => window.limitId === 'codex'))
    ?? entries[0]
}

/** 一个额度组可能包含多个滚动窗口，摘要使用其中当前占用最高的窗口。 */
export function representativeQuotaWindow(entry: AccountQuotaWindowEntry | undefined) {
  return entry?.windows.reduce<AccountQuotaWindow | undefined>((representative, window) => {
    if (typeof window.usedPercent !== 'number')
      return representative
    if (
      !representative
      || typeof representative.usedPercent !== 'number'
      || window.usedPercent > representative.usedPercent
    ) {
      return window
    }
    return representative
  }, undefined)
}

function latestModelUsage(models: readonly AccountModelUsage[]) {
  let latest: AccountModelUsage | undefined
  let latestTimestamp = Number.NEGATIVE_INFINITY

  for (const model of models) {
    const timestamp = Date.parse(model.lastUsedAt)
    if (!Number.isFinite(timestamp) || timestamp <= latestTimestamp)
      continue
    latest = model
    latestTimestamp = timestamp
  }

  return latest
}

function quotaEntryIdentities(entry: AccountQuotaWindowEntry) {
  const identities = new Set<string>()
  addQuotaIdentity(identities, entry.label)
  for (const window of entry.windows) {
    addQuotaIdentity(identities, window.limitId)
    addQuotaIdentity(identities, window.limitName)
  }
  return identities
}

function addQuotaIdentity(identities: Set<string>, value: string | null) {
  const identity = quotaIdentity(value)
  if (identity)
    identities.add(identity)
}

function quotaIdentity(value: string | null) {
  return value?.trim().toLowerCase().replace(/[^a-z0-9]+/g, '') ?? ''
}
