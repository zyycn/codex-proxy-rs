import { describe, expect, it } from 'vitest'

import { dashboardTrendView, formatDashboardCompactNumber } from './presenter'

describe('dashboard presenter', () => {
  it('aggregates adjacent usage buckets without over-counting cached input', () => {
    const view = dashboardTrendView({
      kind: 'usage',
      points: [point({ inputTokensValue: 10, cachedTokensValue: 8 }), point({ cachedTokensValue: 8 })],
      summary: [],
    })

    expect(view.points).toHaveLength(1)
    expect(view.points[0]).toMatchObject({
      inputTokensValue: 10,
      cachedTokensValue: 10,
      uncachedInputTokensValue: 0,
    })
  })

  it('formats large dashboard values compactly', () => {
    expect(formatDashboardCompactNumber(1_250)).toBe('1.25K')
    expect(formatDashboardCompactNumber(-1)).toBe('0')
  })
})

function point(overrides: Partial<ReturnType<typeof pointBase>> = {}) {
  return { ...pointBase(), ...overrides }
}

function pointBase() {
  return {
    time: '12:00',
    requests: '1',
    requestsValue: 1,
    inputTokens: '0',
    inputTokensValue: 0,
    outputTokens: '0',
    outputTokensValue: 0,
    cachedTokens: '0',
    cachedTokensValue: 0,
    cacheHitRateValue: 0,
    tokensValue: 0,
    errors: '0',
    errorsValue: 0,
    latency: '-',
    latencyValue: null,
    maxLatency: '-',
    maxLatencyValue: null,
    minLatency: '-',
    minLatencyValue: null,
    successRate: '100%',
    successRateValue: 100,
  }
}
