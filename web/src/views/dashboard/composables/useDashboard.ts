import { useIntervalFn } from '@vueuse/core'
import { clamp } from 'es-toolkit'
import { onMounted, ref } from 'vue'

import {
  Activity,
  CloudCheck,
  FileText,
  Gauge,
  MonitorCheck,
  RefreshCw,
  Timer,
  Users,
} from '@lucide/vue'

import { getDashboardSummary, getDashboardTrend } from '@/api'
import { withMinimumDuration } from '@/utils/async'
import { formatDateTime } from '@/utils/date'

export function useDashboard(): any {
  const metrics = ref<any[]>(metricCards(emptyDashboardSummary()))
  const trendPoints = ref<any[]>([])
  const trendSummary = ref<any[]>([])
  const healthTimeline = ref<any>(emptyDashboardSummary().healthTimeline)
  const accountUsage = ref<any[]>([])
  const serviceStatuses = ref<any[]>([])
  const usageRecords = ref<any[]>([])
  const poolSummary = ref<any>(null)
  const capacityInfo = ref<any>(null)
  const rotationStrategy = ref<any>(null)
  const loading = ref(false)
  const refreshing = ref(false)
  const trendLoading = ref(false)
  const lastRefreshedAt = ref('')
  const { resume: startAutoRefresh } = useIntervalFn(
    () => {
      void loadDashboardData()
    },
    30000,
    { immediate: false },
  )

  async function loadDashboardData() {
    try {
      loading.value = true
      const summary = await getDashboardSummary()
      applySummary(summary)
    } catch (error) {
      console.error('Failed to load dashboard data:', error)
    } finally {
      loading.value = false
    }
  }

  async function refreshDashboardData() {
    if (loading.value || refreshing.value) return
    refreshing.value = true
    try {
      await withMinimumDuration(async () => {
        const summary = await getDashboardSummary()
        applySummary(summary)
      })
    } catch (error) {
      console.error('Failed to refresh dashboard data:', error)
    } finally {
      refreshing.value = false
    }
  }

  async function loadTrend(tab: string) {
    const kind = trendKindFromTab(tab)
    try {
      trendLoading.value = true
      applyTrend(await getDashboardTrend({ kind }))
    } catch (error) {
      console.error('Failed to load dashboard trend:', error)
    } finally {
      trendLoading.value = false
    }
  }

  function applySummary(summary: any) {
    metrics.value = metricCards(summary)
    applyTrend(summary.trend)
    healthTimeline.value = summary.healthTimeline
    accountUsage.value = summary.accountUsage.map(accountUsageItem)
    serviceStatuses.value = summary.serviceStatuses.map(serviceStatusItem)
    usageRecords.value = summary.usageRecords
    poolSummary.value = summary.poolSummary
    capacityInfo.value = summary.capacityInfo
    rotationStrategy.value = summary.rotationStrategy ?? null
    lastRefreshedAt.value = formatDateTime()
  }

  function emptyDashboardSummary() {
    return {
      cards: {
        accounts: {
          total: 0,
          enabled: 0,
          abnormal: 0,
        },
        traffic: {
          todayRequests: 0,
          yesterdayRequests: 0,
          totalRequests: 0,
          rpm: 0,
          tpm: 0,
        },
        tokens: {
          todayTokens: 0,
          yesterdayTokens: 0,
          totalTokens: 0,
          todayCostUsd: null,
          totalCostUsd: null,
        },
        cache: {
          todayHitRate: null,
          yesterdayHitRate: null,
          totalHitRate: null,
          totalCachedTokens: 0,
          firstTokenLatencyMs: null,
          completionLatencyMs: null,
        },
      },
      trend: {
        kind: 'usage',
        points: [],
        summary: [],
      },
      healthTimeline: {
        title: '请求健康时间线',
        description: '最近 7 天请求可靠性',
        rangeDisplay: '-',
        reliabilityDisplay: '-',
        oldestLabel: '最早',
        newestLabel: '最新',
        points: '',
      },
      accountUsage: [],
      serviceStatuses: [],
      usageRecords: [],
      poolSummary: {
        total: 0,
        active: 0,
        expired: 0,
        quotaExhausted: 0,
        refreshing: 0,
        disabled: 0,
        banned: 0,
      },
      capacityInfo: {
        maxConcurrentPerAccount: 0,
        totalSlots: 0,
        usedSlots: 0,
        availableSlots: 0,
      },
      rotationStrategy: undefined,
    }
  }

  function metricCards(summary: any) {
    const { accounts, traffic, tokens, cache } = summary.cards
    const points = summary.trend.points as any[]
    return [
      {
        title: '账号',
        value: formatNumber(accounts.total),
        icon: Users,
        tone: 'normal',
        details: [
          {
            label: '启用',
            value: formatNumber(accounts.enabled),
            tone: accounts.enabled > 0 ? 'success' : 'normal',
          },
          {
            label: '异常',
            value: formatNumber(accounts.abnormal),
            tone: accounts.abnormal > 0 ? 'danger' : 'normal',
          },
        ],
      },
      {
        title: '今日请求',
        value: formatNumber(traffic.todayRequests),
        icon: Activity,
        tone: 'info',
        sparkline: sparkline(
          points.map((point) => point.requests),
          'info',
        ),
        trend: trendState(traffic.todayRequests, traffic.yesterdayRequests, 'info'),
        details: [
          { label: '总请求', value: formatNumber(traffic.totalRequests), tone: 'info' },
          {
            label: '首 Token',
            value: formatLatency(cache.firstTokenLatencyMs),
            tone: 'info',
          },
        ],
      },
      {
        title: '今日 Token',
        value: formatTokens(tokens.todayTokens),
        icon: FileText,
        tone: 'success',
        sparkline: sparkline(
          points.map((point) => point.tokens),
          'success',
        ),
        trend: trendState(tokens.todayTokens, tokens.yesterdayTokens, 'success'),
        details: [
          { label: '总 Token', value: formatTokens(tokens.totalTokens), tone: 'success' },
          {
            label: '计费',
            value: `${formatCost(tokens.totalCostUsd)} / ${formatCost(tokens.todayCostUsd)}`,
            tone: 'success',
          },
        ],
      },
      {
        title: '今日缓存命中',
        value: formatRate(cache.todayHitRate),
        icon: Timer,
        tone: cache.todayHitRate && cache.todayHitRate > 0 ? 'warning' : 'normal',
        sparkline: sparkline(
          points.map((point) => point.cachedTokens),
          'warning',
        ),
        trend: trendState(cache.todayHitRate ?? 0, cache.yesterdayHitRate ?? 0, 'warning'),
        details: [
          { label: '总缓存命中', value: formatRate(cache.totalHitRate), tone: 'warning' },
          {
            label: '缓存 Token',
            value: formatTokens(cache.totalCachedTokens),
            tone: 'warning',
          },
        ],
      },
    ]
  }

  function sparkline(values: number[], tone: string) {
    return values.some((value) => value > 0) ? { values, tone } : undefined
  }

  function applyTrend(trend: any) {
    trendPoints.value = trend.points
    trendSummary.value = (trend.summary as any[]).map((item) => {
      if (trend.kind === 'latency') {
        return {
          label: item.label,
          value: formatLatency(item.value),
          tone: trendSummaryTone(item.label),
        }
      }
      if (item.ratio !== null && item.ratio !== undefined) {
        return {
          label: item.label,
          value: `${item.ratio.toFixed(1)}%`,
          tone: trendSummaryTone(item.label),
        }
      }
      const formatter =
        trend.kind === 'usage' && item.label !== '总请求' ? formatTokens : formatNumber
      return { label: item.label, value: formatter(item.value), tone: trendSummaryTone(item.label) }
    })
  }

  function accountUsageItem(item: any) {
    const tone = accountTone(item.status)
    const quotaPercent = quotaUsedPercent(item.quotaUsedPercent)
    return {
      name: item.name,
      email: item.email || '-',
      plan: item.plan || 'free',
      requests: formatNumber(item.requests),
      tokens: formatTokens(item.tokens),
      lastUsed: item.lastUsed || '-',
      tone,
      quotaPercent,
      quotaTone: quotaTone(quotaPercent),
    }
  }

  function quotaUsedPercent(value: number | null | undefined): number {
    if (typeof value !== 'number' || !Number.isFinite(value)) return 0
    return clamp(Math.round(value), 0, 100)
  }

  function quotaTone(percent: number) {
    if (percent >= 95) return 'danger'
    if (percent >= 80) return 'warning'
    if (percent > 0) return 'success'
    return 'normal'
  }

  function serviceStatusItem(item: any, index: number) {
    const icons = [MonitorCheck, MonitorCheck, CloudCheck, RefreshCw, Gauge]
    return {
      label: item.label,
      value: item.value || '-',
      detail: item.detail || '-',
      tone: item.tone,
      icon: icons[index] ?? MonitorCheck,
    }
  }

  function trendState(current: number, previous: number, fallbackTone: string) {
    if (current > previous) return { direction: 'up', tone: 'success' }
    if (current < previous) return { direction: 'down', tone: 'danger' }
    return previous > 0 || current > 0 ? { direction: 'flat', tone: fallbackTone } : undefined
  }

  function trendKindFromTab(tab: string) {
    if (tab === '延迟') return 'latency'
    if (tab === '错误') return 'errors'
    return 'usage'
  }

  function trendSummaryTone(label: string) {
    if (label.includes('错误')) return 'danger'
    if (label.includes('最高')) return 'warning'
    if (label.includes('输出') || label.includes('最低') || label.includes('成功')) return 'success'
    if (label.includes('缓存')) return 'normal'
    return 'info'
  }

  function accountTone(status: string) {
    if (status === 'active') return 'success'
    if (status === 'quota_exhausted' || status === 'expired') return 'warning'
    if (status === 'banned' || status === 'disabled') return 'danger'
    if (status === 'refreshing') return 'info'
    return 'normal'
  }

  function formatNumber(num: number): string {
    if (num >= 1_000_000_000) return `${formatCompact(num / 1_000_000_000)}B`
    if (num >= 1_000_000) return `${formatCompact(num / 1_000_000)}M`
    if (num >= 1_000) return `${formatCompact(num / 1_000)}K`
    return String(num)
  }

  function formatTokens(tokens: number): string {
    return formatNumber(tokens)
  }

  function formatCompact(value: number): string {
    const rounded =
      value >= 100 ? value.toFixed(0) : value >= 10 ? value.toFixed(1) : value.toFixed(2)
    return rounded.replace(/\.0+$/, '').replace(/(\.\d*[1-9])0+$/, '$1')
  }

  function formatRate(value: number | null | undefined): string {
    if (value === null || value === undefined) return '-'
    return `${(value * 100).toFixed(1)}%`
  }

  function formatCost(value: number | null | undefined): string {
    if (value === null || value === undefined) return '-'
    if (Math.abs(value) >= 1_000_000_000) return `$${(value / 1_000_000_000).toFixed(2)}B`
    if (Math.abs(value) >= 1_000_000) return `$${(value / 1_000_000).toFixed(2)}M`
    if (Math.abs(value) >= 1_000) return `$${(value / 1_000).toFixed(2)}K`
    return `$${value.toFixed(2)}`
  }

  function formatLatency(ms: number | null | undefined): string {
    if (!ms) return '-'
    if (ms >= 1000) return `${formatCompact(ms / 1000)}s`
    return `${ms}ms`
  }

  onMounted(() => {
    void loadDashboardData()
    startAutoRefresh()
  })

  return {
    loading,
    refreshing,
    trendLoading,
    lastRefreshedAt,
    metrics,
    trendPoints,
    trendSummary,
    healthTimeline,
    accountUsage,
    serviceStatuses,
    usageRecords,
    poolSummary,
    capacityInfo,
    rotationStrategy,
    refresh: refreshDashboardData,
    loadTrend,
  }
}
