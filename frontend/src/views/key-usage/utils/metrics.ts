import type { KeyUsageMetrics } from '@/api'
import { ArrowDown, ArrowUp, Brain, Database, Sparkles } from '@lucide/vue'
import { computed } from 'vue'
import { usageFreshInputTokens } from '@/components/usage-records-table/presenter'
import { t } from '@/i18n'

// 卡片、曲线和图例共用语义与主题色，避免同一指标出现不同颜色。
export const keyUsageTokenMetrics = computed(() => ([
  {
    key: 'inputTokens',
    label: t('usage.metrics.newInput'),
    icon: ArrowDown,
    tone: 'text-z-success-text',
    colorToken: 'color-success-text',
    fallback: '#12B981',
  },
  {
    key: 'outputTokens',
    label: t('usage.metrics.output'),
    icon: ArrowUp,
    tone: 'text-z-warning-text',
    colorToken: 'color-warning-text',
    fallback: '#F59E0B',
  },
  {
    key: 'cacheWriteInputTokens',
    label: t('usage.metrics.cacheCreation'),
    icon: Database,
    tone: 'text-z-info-text',
    colorToken: 'color-info-text',
    fallback: '#5983F4',
  },
  {
    key: 'cachedInputTokens',
    label: t('usage.trends.cacheHit'),
    icon: Sparkles,
    tone: 'text-z-purple-text',
    colorToken: 'color-purple-text',
    fallback: '#722ED1',
  },
  {
    key: 'reasoningOutputTokens',
    label: t('usage.metrics.reasoning'),
    icon: Brain,
    tone: 'text-z-cyan-text',
    colorToken: 'color-cyan-text',
    fallback: '#13C2C2',
  },
] as const))

export function keyUsageTokenValue(metrics: KeyUsageMetrics, key: typeof keyUsageTokenMetrics.value[number]['key']) {
  return key === 'inputTokens'
    ? usageFreshInputTokens(metrics.inputTokens, metrics.cachedInputTokens, metrics.cacheWriteInputTokens)
    : metrics[key]
}
