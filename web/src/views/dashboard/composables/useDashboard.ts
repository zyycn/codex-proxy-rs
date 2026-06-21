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

import { getDiagnostics, getLogs, getUsageSummary, getUsageStats, getAccounts } from '@/api'
import type {
  AccountCapacityInfo,
  AccountPoolSummary,
  AccountUsageItem,
  EventLogItem,
  MetricCardItem,
  ServiceStatusItem,
  TrendPoint,
  TrendSummaryItem,
} from '../types'

export function useDashboard(): {
  loading: Ref<boolean>
  metrics: Ref<MetricCardItem[]>
  trendPoints: Ref<TrendPoint[]>
  trendSummary: Ref<TrendSummaryItem[]>
  accountUsage: Ref<AccountUsageItem[]>
  serviceStatuses: Ref<ServiceStatusItem[]>
  eventLogs: Ref<EventLogItem[]>
  poolSummary: Ref<AccountPoolSummary | null>
  capacityInfo: Ref<AccountCapacityInfo | null>
  rotationStrategy: Ref<string | null>
  refresh: () => Promise<void>
} {
  const metrics = ref<MetricCardItem[]>([])
  const trendPoints = ref<TrendPoint[]>([])
  const trendSummary = ref<TrendSummaryItem[]>([])
  const accountUsage = ref<AccountUsageItem[]>([])
  const serviceStatuses = ref<ServiceStatusItem[]>([])
  const eventLogs = ref<EventLogItem[]>([])
  const poolSummary = ref<AccountPoolSummary | null>(null)
  const capacityInfo = ref<AccountCapacityInfo | null>(null)
  const rotationStrategy = ref<string | null>(null)
  const loading = ref(true)
  const autoRefreshTimer = ref<ReturnType<typeof setInterval> | null>(null)

  async function loadDashboardData() {
    try {
      loading.value = true

      // 并行加载所有数据；日志拉 200 条用于趋势聚合
      const [summary, usageStats, accounts, logs, diagnostics] = await Promise.all([
        getUsageSummary().catch(() => null),
        getUsageStats().catch(() => null),
        getAccounts().catch(() => null),
        getLogs({ limit: 200 }).catch(() => null),
        getDiagnostics().catch(() => null),
      ])

      const totalAccounts = diagnostics?.accounts?.pool?.total ?? accounts?.length ?? 0

      poolSummary.value = diagnostics?.accounts?.pool ?? null
      capacityInfo.value = diagnostics?.accounts?.capacity ?? null
      rotationStrategy.value = diagnostics?.settings?.rotationStrategy ?? null

      updateMetrics(summary, totalAccounts, diagnostics)
      updateAccountUsage(accounts ?? [], usageStats ?? [])
      updateEventLogs((logs ?? []).slice(0, 10))
      updateServiceStatus(diagnostics)
      updateTrendData(logs ?? [], summary)
    } catch (error) {
      console.error('Failed to load dashboard data:', error)
    } finally {
      loading.value = false
    }
  }

  function updateMetrics(summary: any, totalAccounts: number, diagnostics: any) {
    const pool = diagnostics?.accounts?.pool ?? {}
    const activeAccounts = pool.active ?? summary?.accountCount ?? 0
    const errorAccounts = (pool.expired ?? 0) + (pool.disabled ?? 0) + (pool.banned ?? 0)

    const requestCount = summary?.requestCount ?? 0
    const inputTokens = summary?.inputTokens ?? 0
    const outputTokens = summary?.outputTokens ?? 0
    const cachedTokens = summary?.cachedTokens ?? 0
    const reasoningTokens = summary?.reasoningTokens ?? 0
    const emptyResponseCount = summary?.emptyResponseCount ?? 0

    metrics.value = [
      {
        title: '账号',
        value: String(totalAccounts),
        icon: Users,
        tone: 'normal',
        details: [
          { label: '启用', value: String(activeAccounts), tone: activeAccounts > 0 ? 'success' : 'normal' },
          { label: '异常', value: String(errorAccounts), tone: errorAccounts > 0 ? 'danger' : 'normal' },
        ],
      },
      {
        title: '总请求',
        value: formatNumber(requestCount),
        icon: Activity,
        tone: 'info',
        details: [
          { label: '成功', value: formatNumber(requestCount - emptyResponseCount), tone: 'success' },
          { label: '空响应', value: formatNumber(emptyResponseCount), tone: emptyResponseCount > 0 ? 'warning' : 'info' },
        ],
      },
      {
        title: '总 Token',
        value: formatTokens(inputTokens + outputTokens),
        icon: FileText,
        tone: 'success',
        details: [
          { label: '输入', value: formatTokens(inputTokens), tone: 'info' },
          { label: '输出', value: formatTokens(outputTokens), tone: 'success' },
        ],
      },
      {
        title: '缓存命中率',
        value: inputTokens > 0 ? `${((cachedTokens / inputTokens) * 100).toFixed(1)}%` : '—',
        icon: Timer,
        tone: cachedTokens > 0 ? 'success' : 'normal',
        details: [
          { label: '缓存 Token', value: formatTokens(cachedTokens), tone: cachedTokens > 0 ? 'success' : 'normal' },
          { label: '输入总量', value: formatTokens(inputTokens), tone: 'info' },
        ],
      },
    ]
  }

  function updateAccountUsage(accounts: any[], usageStats: any[]) {
    // 构建 accountId → usage 的映射
    const usageByAccount: Record<string, any> = {}
    for (const u of usageStats) {
      usageByAccount[u.accountId] = u
    }

    // 计算用量数据：优先用 usage-stats，回退到 account 自带字段
    const merged = accounts.map((acc) => {
      const usage = usageByAccount[acc.id] ?? {}
      return {
        ...acc,
        _requestCount: usage.requestCount ?? acc.totalRequests ?? 0,
        _inputTokens: usage.inputTokens ?? acc.totalInputTokens ?? 0,
        _outputTokens: usage.outputTokens ?? acc.totalOutputTokens ?? 0,
        _lastUsedAt: usage.lastUsedAt ?? acc.lastUsedAt ?? acc.updatedAt ?? acc.addedAt ?? null,
      }
    })

    // 按请求数降序排列，取前 4
    const sorted = merged
      .sort((a: any, b: any) => b._requestCount - a._requestCount)
      .slice(0, 4)

    const maxTokens = Math.max(...sorted.map((a: any) => a._inputTokens + a._outputTokens), 1)

    accountUsage.value = sorted.map((acc: any) => {
      const totalTokens = acc._inputTokens + acc._outputTokens
      const loadWidth = Math.floor((totalTokens / maxTokens) * 84) // 84px = max bar width

      let tone: AccountUsageItem['tone'] = 'normal'
      if (acc.status === 'active') tone = 'success'
      else if (acc.status === 'quota_exhausted' || acc.status === 'expired') tone = 'warning'
      else if (acc.status === 'banned' || acc.status === 'disabled') tone = 'danger'
      else if (acc.status === 'refreshing') tone = 'info'

      return {
        name: acc.label || (acc.email ? acc.email.split('@')[0] : acc.id.slice(0, 8)),
        email: acc.email || '—',
        plan: acc.planType || 'free',
        requests: formatNumber(acc._requestCount),
        tokens: formatTokens(totalTokens),
        lastUsed: formatRelativeTime(acc._lastUsedAt),
        tone,
        loadWidth,
      }
    })
  }

  function updateEventLogs(logs: any[]) {
    if (!logs.length) {
      eventLogs.value = []
      return
    }

    eventLogs.value = logs.map((log) => {
      let tone: EventLogItem['tone'] = 'info'
      if (log.level === 'warning') tone = 'warning'
      else if (log.level === 'error') tone = 'danger'

      return {
        id: log.id,
        time: formatTime(log.createdAt),
        level: log.level === 'info' ? '信息' : log.level === 'warning' ? '警告' : '错误',
        requestId: log.requestId || '—',
        route: log.route || '—',
        model: log.model || '—',
        statusCode: log.statusCode ? String(log.statusCode) : '—',
        latency: log.latencyMs ? `${(log.latencyMs / 1000).toFixed(2)}s` : '—',
        tone,
      }
    })
  }

  function updateServiceStatus(diagnostics: any) {
    if (!diagnostics) {
      serviceStatuses.value = []
      return
    }

    const pool = diagnostics.accounts?.pool ?? {}
    const capacity = diagnostics.accounts?.capacity ?? {}
    const transport = diagnostics.transport ?? {}
    const fingerprint = transport.fingerprint ?? {}
    const settings = diagnostics.settings ?? {}
    const backendUrl = (transport.backendBaseUrl || '').replace(/^https?:\/\//, '')

    serviceStatuses.value = [
      {
        label: '客户端版本',
        value: fingerprint.appVersion || '—',
        detail: `Build ${fingerprint.buildNumber || '—'}`,
        tone: 'info' as const,
        icon: MonitorCheck,
      },
      {
        label: '平台架构',
        value: fingerprint.platform || '—',
        detail: fingerprint.arch || '—',
        tone: 'info' as const,
        icon: MonitorCheck,
      },
      {
        label: 'Chromium',
        value: fingerprint.chromiumVersion ? `v${fingerprint.chromiumVersion}` : '—',
        detail: fingerprint.originator || '—',
        tone: 'normal' as const,
        icon: CloudCheck,
      },
      {
        label: '更新时间',
        value: fingerprint.updatedAt ? new Date(fingerprint.updatedAt).toLocaleString('zh-CN') : '',
        detail: '',
        tone: 'normal' as const,
        icon: RefreshCw,
      },
      {
        label: 'User Agent',
        value: fingerprint.userAgent || `${fingerprint.originator || 'Codex Desktop'}/${fingerprint.appVersion || '?'} (${fingerprint.platform || '?'}; ${fingerprint.arch || '?'})`,
        detail: '',
        tone: 'normal' as const,
        icon: Gauge,
      },
    ]
  }

  function updateTrendData(logs: any[], summary: any) {
    if (!logs.length) {
      trendPoints.value = []
      trendSummary.value = [
        { label: '输入', value: '—', tone: 'info' as const },
        { label: '输出', value: '—', tone: 'success' as const },
        { label: '缓存', value: '—', tone: 'normal' as const },
      ]
      return
    }

    // 按小时分桶，取最近 24 个桶
    const now = Date.now()
    const buckets: Record<string, { requests: number; inputTokens: number; outputTokens: number; errors: number; latencySum: number; latencyCount: number }> = {}
    for (let h = 23; h >= 0; h--) {
      const key = new Date(now - h * 3600000).toISOString().slice(11, 13)
      buckets[key] = { requests: 0, inputTokens: 0, outputTokens: 0, errors: 0, latencySum: 0, latencyCount: 0 }
    }

    for (const log of logs) {
      const hour = log.createdAt?.slice(11, 13)
      if (!hour || !buckets[hour]) continue
      buckets[hour].requests++
      const usage = log.metadata?.usage
      if (usage) {
        buckets[hour].inputTokens += usage.inputTokens || 0
        buckets[hour].outputTokens += usage.outputTokens || 0
      }
      const code = log.statusCode
      if (code && (code >= 400 || code < 0)) buckets[hour].errors++
      if (log.latencyMs) {
        buckets[hour].latencySum += log.latencyMs
        buckets[hour].latencyCount++
      }
    }

    const entries = Object.entries(buckets)
    trendPoints.value = entries.map(([time, b]) => ({
      time,
      requests: b.requests,
      tokens: b.inputTokens + b.outputTokens,
      errors: b.errors,
      latency: b.latencyCount > 0 ? Math.round(b.latencySum / b.latencyCount) : 0,
    }))

    const totalInput = entries.reduce((s, [, b]) => s + b.inputTokens, 0)
    const totalOutput = entries.reduce((s, [, b]) => s + b.outputTokens, 0)
    const totalErrors = entries.reduce((s, [, b]) => s + b.errors, 0)

    trendSummary.value = [
      { label: '输入', value: totalInput > 0 ? formatTokens(totalInput) : '—', tone: 'info' as const },
      { label: '输出', value: totalOutput > 0 ? formatTokens(totalOutput) : '—', tone: 'success' as const },
      { label: '缓存', value: formatTokens(summary?.cachedTokens ?? 0), tone: (summary?.cachedTokens ?? 0) > 0 ? 'success' as const : 'normal' as const },
    ]
  }

  function formatNumber(num: number): string {
    if (num >= 1_000_000) return `${(num / 1_000_000).toFixed(1)}M`
    if (num >= 1_000) return `${(num / 1_000).toFixed(1)}K`
    return String(num)
  }

  function formatTokens(tokens: number): string {
    if (tokens >= 1_000_000) return `${(tokens / 1_000_000).toFixed(2)}M`
    if (tokens >= 1_000) return `${(tokens / 1_000).toFixed(1)}K`
    return String(tokens)
  }

  function formatLatency(ms: number | undefined): string {
    if (!ms) return '—'
    if (ms >= 1000) return `${(ms / 1000).toFixed(2)}s`
    return `${Math.round(ms)}ms`
  }

  function formatRelativeTime(dateStr: string | undefined): string {
    if (!dateStr) return '—'
    const date = new Date(dateStr)
    const now = new Date()
    const diff = now.getTime() - date.getTime()
    const minutes = Math.floor(diff / 60000)
    const hours = Math.floor(diff / 3600000)
    const days = Math.floor(diff / 86400000)

    if (minutes < 1) return '刚刚'
    if (minutes < 60) return `${minutes}分钟前`
    if (hours < 24) return `${hours}小时前`
    if (days < 7) return `${days}天前`
    return date.toLocaleDateString('zh-CN')
  }

  function formatTime(dateStr: string): string {
    const date = new Date(dateStr)
    return date.toLocaleTimeString('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit' })
  }

  // 启动自动刷新
  function startAutoRefresh() {
    stopAutoRefresh()
    autoRefreshTimer.value = setInterval(() => {
      loadDashboardData()
    }, 30000) // 30秒刷新一次
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
    metrics,
    trendPoints,
    trendSummary,
    accountUsage,
    serviceStatuses,
    eventLogs,
    poolSummary,
    capacityInfo,
    rotationStrategy,
    refresh: loadDashboardData,
  }
}
