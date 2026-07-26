// 详情弹窗共享的字段展示约定：占位符、标签/值样式，供弹窗与其子组件复用。
export const fieldLabelClass = 'text-[11px] leading-none font-bold text-(--cp-text-muted)'
export const fieldValueBaseClass
  = 'mt-1.5 mb-0 min-w-0 truncate text-[12px] leading-none font-bold text-(--cp-text-primary)'

export function displayValue(value: unknown) {
  if (value === undefined || value === null || value === '')
    return '—'
  return String(value)
}

export function fieldValueClass(mono?: boolean) {
  return [fieldValueBaseClass, mono ? 'font-mono tabular-nums' : undefined]
}
