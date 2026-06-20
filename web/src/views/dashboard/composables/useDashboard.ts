import { onMounted, onUnmounted, ref } from 'vue'
import type { Ref } from 'vue'

import {
  Activity,
  Boxes,
  CloudCheck,
  FileText,
  MonitorCheck,
  RefreshCw,
  ScrollText,
  Timer,
  Users,
} from '@lucide/vue'

import { getDiagnostics, getLogs, getUsageSummary, getAccounts } from '@/api'
import type {
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
  refresh: () => Promise<void>
} {
  const metrics = ref<MetricCardItem[]>([])
  const trendPoints = ref<TrendPoint[]>([])
  const trendSummary = ref<TrendSummaryItem[]>([])
  const accountUsage = ref<AccountUsageItem[]>([])
  const serviceStatuses = ref<ServiceStatusItem[]>([])
  const eventLogs = ref<EventLogItem[]>([])
  const loading = ref(true)
  const autoRefreshTimer = ref<ReturnType<typeof setInterval> | null>(null)

  async function loadDashboardData() {
    try {
      loading.value = true

      // 并行加载所有数据
      const [summary, accounts, logs, diagnostics] = await Promise.all([
        getUsageSummary().catch(() => null),
        getAccounts().catch(() => null),
        getLogs({ limit: 10 }).catch(() => null),
        getDiagnostics().catch(() => null),
      ])

      // 更新指标卡片
      if (summary) {
        updateMetrics(summary, accounts?.length || 0)
      }

      // 更新账号使用情况
      if (accounts) {
        updateAccountUsage(accounts.slice(0, 4))
      }

      // 更新事件日志
      if (logs) {
        updateEventLogs(logs)
      }

      // 更新服务状态
      if (diagnostics) {
        updateServiceStatus(diagnostics)
      }

      // 更新趋势数据（使用模拟数据，实际应从后端获取）
      updateTrendData()
    } catch (error) {
      console.error('Failed to load dashboard data:', error)
    } finally {
      loading.value = false
    }
  }

  function updateMetrics(summary: any, totalAccounts: number) {
    const activeAccounts = summary.activeAccounts || 0
    const errorAccounts = totalAccounts - activeAccounts

    metrics.value = [
      {
        title: '账号',
        value: String(totalAccounts),
        icon: Users,
        tone: 'normal',
        details: [
          { label: '启用', value: String(activeAccounts), tone: activeAccounts > 0 ? 'success' : 'normal' },
          { label: '错误', value: String(errorAccounts), tone: errorAccounts > 0 ? 'danger' : 'normal' },
        ],
      },
      {
        title: '今日请求',
        value: formatNumber(summary.todayRequests || 0),
        icon: Activity,
        tone: 'info',
        details: [
          { label: '今日', value: formatNumber(summary.todayRequests || 0), tone: 'info' },
          { label: '总计', value: formatNumber(summary.totalRequests || 0), tone: 'info' },
        ],
      },
      {
        title: '总 Token',
        value: formatTokens(summary.totalInputTokens + summary.totalOutputTokens),
        icon: FileText,
        tone: 'success',
        details: [
          { label: '今日', value: formatTokens(summary.todayInputTokens + summary.todayOutputTokens), tone: 'success' },
          { label: '总计', value: formatTokens(summary.totalInputTokens + summary.totalOutputTokens), tone: 'success' },
        ],
      },
      {
        title: '平均响应',
        value: formatLatency(summary.avgLatencyMs),
        icon: Timer,
        tone: summary.avgLatencyMs > 3000 ? 'warning' : 'success',
        details: [
          { label: '平均', value: formatLatency(summary.avgLatencyMs), tone: summary.avgLatencyMs > 3000 ? 'warning' : 'success' },
          { label: '错误率', value: `${(summary.errorRate * 100).toFixed(2)}%`, tone: summary.errorRate > 0.01 ? 'warning' : 'success' },
        ],
      },
    ]
  }

  function updateAccountUsage(accounts: any[]) {
    accountUsage.value = accounts.map((acc) => {
      const totalTokens = acc.totalInputTokens + acc.totalOutputTokens
      const maxTokens = Math.max(...accounts.map((a: any) => a.totalInputTokens + a.totalOutputTokens))
      const loadWidth = maxTokens > 0 ? Math.floor((totalTokens / maxTokens) * 100) : 0

      let tone: AccountUsageItem['tone'] = 'normal'
      if (acc.status === 'active') tone = 'success'
      else if (acc.status === 'quota_exhausted' || acc.status === 'expired') tone = 'warning'
      else if (acc.status === 'banned' || acc.status === 'disabled') tone = 'danger'

      return {
        name: acc.label || acc.email.split('@')[0],
        email: acc.email,
        plan: acc.planType || 'free',
        requests: formatNumber(acc.totalRequests),
        tokens: formatTokens(totalTokens),
        lastUsed: formatRelativeTime(acc.lastUsedAt),
        tone,
        loadWidth,
      }
    })
  }

  function updateEventLogs(logs: any[]) {
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
    serviceStatuses.value = [
      {
        label: '上游连接',
        value: diagnostics.database.connected ? '正常' : '断开',
        detail: '—',
        tone: diagnostics.database.connected ? 'success' : 'danger',
        icon: CloudCheck,
      },
      {
        label: '模型目录',
        value: '已同步',
        detail: '—',
        tone: 'info',
        icon: Boxes,
      },
      {
        label: '自动刷新',
        value: '开启',
        detail: `${diagnostics.accounts.active} 活跃`,
        tone: 'success',
        icon: RefreshCw,
      },
      {
        label: '事件记录',
        value: '开启',
        detail: `已存 ${formatNumber(diagnostics.requests.total)}`,
        tone: 'success',
        icon: ScrollText,
      },
      {
        label: '系统版本',
        value: diagnostics.version,
        detail: diagnostics.environment,
        tone: 'normal',
        icon: MonitorCheck,
      },
    ]
  }

  function updateTrendData() {
    // 模拟趋势数据，实际应从后端 API 获取
    const hours = ['00', '04', '08', '12', '16', '20', '24']
    trendPoints.value = hours.map(time => ({
      time,
      requests: Math.floor(Math.random() * 1000),
      tokens: Math.floor(Math.random() * 50000),
      errors: Math.floor(Math.random() * 10),
    }))

    const successRate = 99.5 + Math.random() * 0.5
    const peak = Math.max(...trendPoints.value.map(p => p.requests))
    const slowRequests = Math.floor(Math.random() * 100)

    trendSummary.value = [
      { label: '成功率', value: `${successRate.toFixed(2)}%`, tone: 'success' },
      { label: '峰值', value: formatNumber(peak), tone: 'info' },
      { label: '慢请求', value: String(slowRequests), tone: slowRequests > 50 ? 'warning' : 'info' },
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
    refresh: loadDashboardData,
  }
}
