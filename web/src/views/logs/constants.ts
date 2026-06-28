export const logColumns = [
  {
    key: 'createdAtDisplay',
    label: '时间',
    width: '176px',
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  { key: 'level', label: '级别', width: '96px', ellipsis: false },
  {
    key: 'kind',
    label: '类型',
    width: '128px',
    cellClass: 'font-[650] text-(--cp-text-secondary)',
  },
  {
    key: 'requestId',
    label: '请求 ID',
    minWidth: '220px',
    flex: 1.1,
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'route',
    label: '路由',
    minWidth: '156px',
    flex: 0.8,
    cellClass: 'font-mono text-[12px] font-[650]',
  },
  {
    key: 'statusCode',
    label: '状态',
    width: '92px',
    align: 'center' as const,
    ellipsis: false,
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums',
  },
  {
    key: 'latencyMs',
    label: '延迟',
    width: '96px',
    align: 'right' as const,
    ellipsis: false,
    format: (value?: number | null) =>
      value === undefined || value === null ? '—' : `${value} ms`,
    cellClass: 'font-mono text-[12px] font-[650] tabular-nums text-(--cp-text-secondary)',
  },
  { key: 'message', label: '消息', minWidth: '260px', flex: 1.35 },
  {
    key: 'actions',
    label: '操作',
    width: '92px',
    align: 'center' as const,
    ellipsis: false,
    headerClass: '!px-4',
    cellClass: '!px-4',
  },
]

export const levelColors: Record<string, { bg: string; text: string }> = {
  info: { bg: 'bg-(--cp-info-bg)', text: 'text-(--cp-info-text)' },
  warn: { bg: 'bg-(--cp-warning-bg)', text: 'text-(--cp-warning-text)' },
  error: { bg: 'bg-(--cp-danger-bg)', text: 'text-(--cp-danger-text)' },
  debug: { bg: 'bg-(--cp-bg-subtle)', text: 'text-(--cp-text-secondary)' },
}

export const levelOptions = [
  { label: '全部级别', value: '' },
  { label: '信息', value: 'info' },
  { label: '错误', value: 'error' },
]

export const levelLabels: Record<string, string> = {
  debug: '调试',
  info: '信息',
  warn: '警告',
  error: '错误',
}
