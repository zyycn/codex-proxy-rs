import type { AccountQuotaWindow } from './quota'

import { describe, expect, it } from 'vitest'
import {

  orderedPanelQuotaWindows,
  quotaWindowBarClass,
  quotaWindowPercent,
  quotaWindowPercentTextClass,
  visibleSummaryQuotaWindows,
} from './quota'

describe('account quota presenter', () => {
  it('uses backend window groups instead of inferring semantics from duration', () => {
    const windows = [window('other', 7), window('monthly', 20), window('shortTerm', 30)]

    expect(visibleSummaryQuotaWindows(windows).map(item => item.group)).toEqual([
      'shortTerm',
      'monthly',
    ])
    expect(orderedPanelQuotaWindows(windows).map(item => item.group)).toEqual([
      'monthly',
      'shortTerm',
      'other',
    ])
  })

  it('clamps percentages and applies stable threshold tones', () => {
    expect(quotaWindowPercent(window('shortTerm', 120))).toBe(100)
    expect(quotaWindowBarClass(window('shortTerm', 95))).toBe('bg-(--cp-danger)')
    expect(quotaWindowPercentTextClass(window('shortTerm', 80))).toBe(
      'text-(--cp-warning-text)',
    )
    expect(quotaWindowBarClass(window('shortTerm', null))).toBe(
      'bg-(--cp-default-border-hover)',
    )
  })
})

function window(group: AccountQuotaWindow['group'], usedPercent: number | null): AccountQuotaWindow {
  return {
    key: `${group}-${usedPercent}`,
    group,
    windowSeconds: null,
    labelDisplay: group,
    usedPercent,
    usedPercentDisplay: usedPercent === null ? '-' : `${usedPercent}%`,
    resetAtDisplay: '-',
    windowUsedDisplay: '-',
  }
}
