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

import type {
  AccountUsageItem,
  EventLogItem,
  MetricCardItem,
  ServiceStatusItem,
  TrendPoint,
  TrendSummaryItem,
} from '../types'

export function useDashboard() {
  const metrics: MetricCardItem[] = [
    {
      title: '账号',
      value: '33',
      icon: Users,
      tone: 'normal',
      details: [
        { label: '启用', value: '1', tone: 'normal' },
        { label: '错误', value: '31', tone: 'danger' },
      ],
    },
    {
      title: '今日请求',
      value: '0',
      icon: Activity,
      tone: 'info',
      details: [
        { label: '今日', value: '0', tone: 'info' },
        { label: '总计', value: '3,363', tone: 'info' },
      ],
    },
    {
      title: '总 Token',
      value: '401.29M',
      icon: FileText,
      tone: 'success',
      details: [
        { label: '今日', value: '0', tone: 'success' },
        { label: '总计', value: '401.29M', tone: 'success' },
      ],
    },
    {
      title: '平均响应',
      value: '21.71s',
      icon: Timer,
      tone: 'warning',
      details: [
        { label: '今日', value: '21.71s', tone: 'warning' },
        { label: '目标', value: '< 3s', tone: 'warning' },
      ],
    },
  ]

  const trendPoints: TrendPoint[] = [
    { time: '00', requests: 0, tokens: 0, errors: 0 },
    { time: '04', requests: 0, tokens: 0, errors: 0 },
    { time: '08', requests: 0, tokens: 0, errors: 0 },
    { time: '12', requests: 0, tokens: 0, errors: 0 },
    { time: '16', requests: 0, tokens: 0, errors: 0 },
    { time: '20', requests: 0, tokens: 0, errors: 0 },
    { time: '24', requests: 0, tokens: 0, errors: 0 },
  ]

  const trendSummary: TrendSummaryItem[] = [
    { label: '成功率', value: '99.88%', tone: 'success' },
    { label: '峰值', value: '15.2K', tone: 'info' },
    { label: '慢请求', value: '42', tone: 'warning' },
  ]

  const accountUsage: AccountUsageItem[] = [
    {
      name: 'Amy Ops',
      email: 'amy.ops@example.com',
      plan: 'plus',
      requests: '12.8K',
      tokens: '382K',
      lastUsed: '2分钟前',
      tone: 'info',
      loadWidth: 84,
    },
    {
      name: 'Team Codex',
      email: 'team-codex@example.com',
      plan: 'pro',
      requests: '9.4K',
      tokens: '241K',
      lastUsed: '8分钟前',
      tone: 'normal',
      loadWidth: 70,
    },
    {
      name: 'Build Bot',
      email: 'build@proxy.local',
      plan: 'team',
      requests: '7.1K',
      tokens: '198K',
      lastUsed: '16分钟前',
      tone: 'success',
      loadWidth: 58,
    },
    {
      name: 'Reviewer',
      email: 'reviewer@example.com',
      plan: 'plus',
      requests: '6.8K',
      tokens: '176K',
      lastUsed: '24分钟前',
      tone: 'warning',
      loadWidth: 48,
    },
  ]

  const serviceStatuses: ServiceStatusItem[] = [
    { label: '上游连接', value: '正常', detail: '23ms', tone: 'success', icon: CloudCheck },
    { label: '模型目录', value: '已同步', detail: '58 个模型', tone: 'info', icon: Boxes },
    { label: '自动刷新', value: '开启', detail: '2 并发', tone: 'success', icon: RefreshCw },
    { label: '事件记录', value: '关闭', detail: '已存 1.8K', tone: 'warning', icon: ScrollText },
    { label: '客户端版本', value: '可用', detail: '26.519', tone: 'normal', icon: MonitorCheck },
  ]

  const eventLogs: EventLogItem[] = [
    {
      id: 'evt_01',
      time: '14:30:25',
      level: '信息',
      requestId: 'req_01HW9K7QY2',
      route: '/v1/responses',
      model: 'gpt-5.5',
      statusCode: '200',
      latency: '1.25s',
      tone: 'info',
    },
    {
      id: 'evt_02',
      time: '14:30:15',
      level: '警告',
      requestId: 'req_01HW9K6N4F',
      route: '/v1/chat/completions',
      model: 'gpt-5.5',
      statusCode: '429',
      latency: '2.15s',
      tone: 'warning',
    },
    {
      id: 'evt_03',
      time: '14:30:10',
      level: '错误',
      requestId: 'req_01HW9K5Z7A',
      route: '/v1/responses',
      model: 'gpt-5.5',
      statusCode: '502',
      latency: '5.32s',
      tone: 'danger',
    },
    {
      id: 'evt_04',
      time: '14:29:56',
      level: '信息',
      requestId: 'req_01HW9K4C3Q',
      route: '/v1/models',
      model: 'catalog',
      statusCode: '200',
      latency: '0.84s',
      tone: 'info',
    },
  ]

  return {
    metrics,
    trendPoints,
    trendSummary,
    accountUsage,
    serviceStatuses,
    eventLogs,
  }
}
