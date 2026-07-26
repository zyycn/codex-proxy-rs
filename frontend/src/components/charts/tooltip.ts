// ECharts tooltip 回调参数的通用安全取值：参数形状不受控（数组/对象混合），
// 统一在此收敛类型判断。
export function tooltipRows(params: unknown): Record<string, unknown>[] {
  const values = Array.isArray(params) ? params : [params]
  return values.filter(
    (value): value is Record<string, unknown> => typeof value === 'object' && value !== null,
  )
}

export function tooltipIndex(source: unknown) {
  if (typeof source !== 'object' || source === null || !('dataIndex' in source))
    return -1
  return typeof source.dataIndex === 'number' ? source.dataIndex : -1
}
