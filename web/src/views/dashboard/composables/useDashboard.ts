import { onMounted, onUnmounted, ref } from 'vue'
import type { Ref } from 'vue'

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
import type {
  DashboardAccountUsage,
  DashboardHealthTimeline,
  DashboardServiceStatus,
  DashboardSummary,
  DashboardTrend,
  DashboardTrendKind,
} from '@/api'
import type {
  AccountCapacityInfo,
  AccountPoolSummary,
  AccountUsageItem,
  EventLogItem,
  MetricCardItem,
  SemanticTone,
  ServiceStatusItem,
  TrendPoint,
  TrendSummaryItem,
} from '../types'
import { withMinimumDuration } from '@/utils/async'

export function useDashboard(): {
  loading: Ref<boolean>
  trendLoading: Ref<boolean>
  metrics: Ref<MetricCardItem[]>
  trendPoints: Ref<TrendPoint[]>
  trendSummary: Ref<TrendSummaryItem[]>
  healthTimeline: Ref<DashboardHealthTimeline>
  accountUsage: Ref<AccountUsageItem[]>
  serviceStatuses: Ref<ServiceStatusItem[]>
  eventLogs: Ref<EventLogItem[]>
  poolSummary: Ref<AccountPoolSummary | null>
  capacityInfo: Ref<AccountCapacityInfo | null>
  rotationStrategy: Ref<string | null>
  refresh: () => Promise<void>
  loadTrend: (tab: string) => Promise<void>
} {
  const metrics = ref<MetricCardItem[]>(metricCards(emptyDashboardSummary()))
  const trendPoints = ref<TrendPoint[]>([])
  const trendSummary = ref<TrendSummaryItem[]>([])
  const healthTimeline = ref<DashboardHealthTimeline>(emptyDashboardSummary().healthTimeline)
  const accountUsage = ref<AccountUsageItem[]>([])
  const serviceStatuses = ref<ServiceStatusItem[]>([])
  const eventLogs = ref<EventLogItem[]>([])
  const poolSummary = ref<AccountPoolSummary | null>(null)
  const capacityInfo = ref<AccountCapacityInfo | null>(null)
  const rotationStrategy = ref<string | null>(null)
  const loading = ref(false)
  const trendLoading = ref(false)
  const autoRefreshTimer = ref<ReturnType<typeof setInterval> | null>(null)

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
    if (loading.value) return
    await withMinimumDuration(loadDashboardData)
  }

  async function loadTrend(tab: string) {
    const kind = trendKindFromTab(tab)
    try {
      trendLoading.value = true
      applyTrend(await getDashboardTrend(kind))
    } catch (error) {
      console.error('Failed to load dashboard trend:', error)
    } finally {
      trendLoading.value = false
    }
  }

  function applySummary(summary: DashboardSummary) {
    metrics.value = metricCards(summary)
    applyTrend(summary.trend)
    healthTimeline.value = summary.healthTimeline
    accountUsage.value = summary.accountUsage.map(accountUsageItem)
    serviceStatuses.value = summary.serviceStatuses.map(serviceStatusItem)
    eventLogs.value = summary.eventLogs.map((log) => ({
      id: log.id,
      time: log.time,
      level: levelLabel(log.level),
      requestId: log.requestId || '-',
      route: log.route || '-',
      model: log.model || '-',
      statusCode: log.statusCode !== undefined ? String(log.statusCode) : '-',
      latency: log.latencyMs !== undefined ? formatLatency(log.latencyMs) : '-',
      tone: eventTone(log.level, log.statusCode),
    }))
    poolSummary.value = summary.poolSummary
    capacityInfo.value = summary.capacityInfo
    rotationStrategy.value = summary.rotationStrategy ?? null
  }

  function emptyDashboardSummary(): DashboardSummary {
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
      eventLogs: [],
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

  function metricCards(summary: DashboardSummary): MetricCardItem[] {
    const { accounts, traffic, tokens, cache } = summary.cards
    const points = summary.trend.points
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
        title: '今日流量',
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
            label: 'RPM / TPM',
            value: `${formatNumber(traffic.rpm)} / ${formatTokens(traffic.tpm)}`,
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

  function sparkline(values: number[], tone: SemanticTone): MetricCardItem['sparkline'] {
    return values.some((value) => value > 0) ? { values, tone } : undefined
  }

  function applyTrend(trend: DashboardTrend) {
    trendPoints.value = trend.points
    trendSummary.value = trend.summary.map((item) => {
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

  function accountUsageItem(item: DashboardAccountUsage): AccountUsageItem {
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
    return Math.max(0, Math.min(100, Math.round(value)))
  }

  function quotaTone(percent: number): SemanticTone {
    if (percent >= 95) return 'danger'
    if (percent >= 80) return 'warning'
    if (percent > 0) return 'success'
    return 'normal'
  }

  function serviceStatusItem(item: DashboardServiceStatus, index: number): ServiceStatusItem {
    const icons = [MonitorCheck, MonitorCheck, CloudCheck, RefreshCw, Gauge]
    return {
      label: item.label,
      value: item.value || '-',
      detail: item.detail || '-',
      tone: item.tone,
      icon: icons[index] ?? MonitorCheck,
    }
  }

  function trendState(
    current: number,
    previous: number,
    fallbackTone: SemanticTone,
  ): MetricCardItem['trend'] {
    if (current > previous) return { direction: 'up', tone: 'success' }
    if (current < previous) return { direction: 'down', tone: 'danger' }
    return previous > 0 || current > 0 ? { direction: 'flat', tone: fallbackTone } : undefined
  }

  function trendKindFromTab(tab: string): DashboardTrendKind {
    if (tab === '延迟') return 'latency'
    if (tab === '错误') return 'errors'
    return 'usage'
  }

  function trendSummaryTone(label: string): SemanticTone {
    if (label.includes('错误')) return 'danger'
    if (label.includes('最高')) return 'warning'
    if (label.includes('输出') || label.includes('最低') || label.includes('成功')) return 'success'
    if (label.includes('缓存')) return 'normal'
    return 'info'
  }

  function accountTone(status: DashboardAccountUsage['status']): SemanticTone {
    if (status === 'active') return 'success'
    if (status === 'quota_exhausted' || status === 'expired') return 'warning'
    if (status === 'banned' || status === 'disabled') return 'danger'
    if (status === 'refreshing') return 'info'
    return 'normal'
  }

  function eventTone(level: string, statusCode?: number): SemanticTone {
    if (level === 'warn') return 'warning'
    if (level === 'error' || (typeof statusCode === 'number' && statusCode >= 400)) return 'danger'
    if (level === 'debug') return 'normal'
    return 'info'
  }

  function levelLabel(level: string): string {
    const labels: Record<string, string> = {
      debug: '调试',
      info: '信息',
      warn: '警告',
      error: '错误',
    }
    return labels[level] ?? level
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

  function startAutoRefresh() {
    stopAutoRefresh()
    autoRefreshTimer.value = setInterval(() => {
      loadDashboardData()
    }, 30000)
  }

  function stopAutoRefresh() {
    if (autoRefreshTimer.value) {
      clearInterval(autoRefreshTimer.value)
      autoRefreshTimer.value = null
    }
  }

  onMounted(() => {
    loadDashboardData()
    startAutoRefresh()
  })

  onUnmounted(() => {
    stopAutoRefresh()
  })

  return {
    loading,
    trendLoading,
    metrics,
    trendPoints,
    trendSummary,
    healthTimeline,
    accountUsage,
    serviceStatuses,
    eventLogs,
    poolSummary,
    capacityInfo,
    rotationStrategy,
    refresh: refreshDashboardData,
    loadTrend,
  }
}
