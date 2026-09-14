import type { Component } from 'vue'

export type MetricTone = 'normal' | 'info' | 'success' | 'warning' | 'danger'

export interface MetricCardView {
  title: string
  value: string
  valueRaw?: number | null
  valueFormatter?: (value: number) => string
  icon: Component
  tone: MetricTone
  sparkline?: {
    values: number[]
    tone: MetricTone
  }
  trend?: {
    direction: 'up' | 'down' | 'flat'
    tone: MetricTone
  }
  details: Array<{
    label: string
    value: string
    tone?: MetricTone
  }>
}

export type RequestTrendKind = 'usage' | 'latency' | 'errors'

export interface RequestTrendPoint {
  time: string
  bucket: string
  label: string
  requests: string
  requestsValue: number
  inputTokens: string
  inputTokensValue: number
  outputTokens: string
  outputTokensValue: number
  cachedTokens: string
  cachedTokensValue: number
  uncachedInputTokens?: string
  uncachedInputTokensValue?: number
  effectiveTokens?: string
  effectiveTokensValue?: number | null
  cacheHitRate?: string
  cacheHitRateValue: number
  tokensValue: number
  errors: string
  errorsValue: number
  latency: string
  latencyValue: number | null
  maxLatency: string
  maxLatencyValue: number | null
  minLatency: string
  minLatencyValue: number | null
  successRate: string
  successRateValue: number | null
  firstTokenP50Ms: number | null
  firstTokenP95Ms: number | null
  latencyP95Ms: number | null
  outputThroughputP50: number | null
}

export interface RequestTrendSummaryItem {
  label: string
  value: string
  tone: MetricTone
  colorVar: string
}

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
