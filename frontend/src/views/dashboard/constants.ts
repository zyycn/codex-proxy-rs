import type { MetricTone } from './presenter'

// tone 到 Tailwind 类的唯一映射；仪表盘各卡片共用。
export const metricToneIconClasses: Record<MetricTone, string> = {
  normal: 'bg-cp-cyan-container text-cp-cyan-on-container',
  info: 'bg-cp-blue-container text-cp-blue-on-container',
  success: 'bg-cp-green-container text-cp-green-on-container',
  warning: 'bg-cp-orange-container text-cp-orange-on-container',
  danger: 'bg-cp-error-container text-cp-error-on-container',
}

export const metricToneValueClasses: Record<MetricTone, string> = {
  normal: 'text-cp-text',
  info: 'text-cp-info-text',
  success: 'text-cp-success-text',
  warning: 'text-cp-warning-text',
  danger: 'text-cp-error-text',
}
