import type { Component } from 'vue'

export type SemanticTone = 'normal' | 'info' | 'success' | 'warning' | 'danger'

export interface MetricDetail {
  label: string
  value: string
  tone?: SemanticTone
}

export interface MetricCardItem {
  title: string
  value: string
  icon: Component
  tone: SemanticTone
  details: MetricDetail[]
}

export interface TrendPoint {
  time: string
  requests: number
  tokens: number
  errors: number
  latency: number
}

export interface TrendSummaryItem {
  label: string
  value: string
  tone: SemanticTone
}

export interface AccountUsageItem {
  name: string
  email: string
  plan: string
  requests: string
  tokens: string
  lastUsed: string
  tone: SemanticTone
  loadWidth: number
}

export interface ServiceStatusItem {
  label: string
  value: string
  detail: string
  tone: SemanticTone
  icon: Component
}

export interface EventLogItem {
  id: string
  time: string
  level: string
  requestId: string
  route: string
  model: string
  statusCode: string
  latency: string
  tone: SemanticTone
}

export interface AccountPoolSummary {
  total: number
  active: number
  expired: number
  quotaExhausted: number
  refreshing: number
  disabled: number
  banned: number
}

export interface AccountCapacityInfo {
  maxConcurrentPerAccount: number
  totalSlots: number
  usedSlots: number
  availableSlots: number
}

export interface AccountStatusRow {
  label: string
  description: string
  value: string
  tone: SemanticTone
  icon: Component
}

export interface ScheduleStat {
  label: string
  value: string
}
