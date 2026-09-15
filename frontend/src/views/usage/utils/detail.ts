// 详情弹窗共享的字段展示约定：占位符、标签/值样式，供弹窗与其子组件复用。
export const fieldLabelClass = 'text-cp-xs leading-none font-bold text-cp-text-quaternary'
export const fieldValueBaseClass
  = 'mt-1.5 mb-0 min-w-0 truncate text-cp-sm leading-none font-bold text-cp-text'

export function displayValue(value: unknown) {
  if (value === undefined || value === null || value === '')
    return '—'
  return String(value)
}

export function fieldValueClass(mono?: boolean) {
  return [fieldValueBaseClass, mono ? 'font-mono tabular-nums' : undefined]
}
