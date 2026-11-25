export type MetricTone = 'normal' | 'info' | 'success' | 'warning' | 'danger'

// tone 到 Tailwind 类的唯一映射；仪表盘各卡片共用。
export const metricToneIconClasses: Record<MetricTone, string> = {
  normal: 'bg-z-cyan-container text-z-cyan-on-container',
  info: 'bg-z-blue-container text-z-blue-on-container',
  success: 'bg-z-green-container text-z-green-on-container',
  warning: 'bg-z-orange-container text-z-orange-on-container',
  danger: 'bg-z-error-container text-z-error-on-container',
}

export const metricToneValueClasses: Record<MetricTone, string> = {
  normal: 'text-z-text',
  info: 'text-z-info-text',
  success: 'text-z-success-text',
  warning: 'text-z-warning-text',
  danger: 'text-z-error-text',
}
