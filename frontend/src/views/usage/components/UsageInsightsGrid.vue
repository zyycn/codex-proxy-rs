<script setup lang="ts">
import type { UsageQueryState } from '../composables/useUsageInsights'
import type { UsageCostMetric, UsageDiagnosticsResponse, UsageMetricResponse, UsagePerformanceMetric } from '@/api'
import { useI18n } from 'vue-i18n'
import UsageCostCard from './UsageCostCard.vue'
import UsageDiagnosticCard from './UsageDiagnosticCard.vue'
import UsageHealthCard from './UsageHealthCard.vue'
import UsagePerformanceCard from './UsagePerformanceCard.vue'

defineProps<{
  health: UsageQueryState<UsageMetricResponse<'health'>>
  diagnostics: UsageQueryState<UsageDiagnosticsResponse>
  performance: UsageQueryState<UsageMetricResponse<UsagePerformanceMetric>>
  cost: UsageQueryState<UsageMetricResponse<UsageCostMetric>>
}>()

const { t } = useI18n({ useScope: 'global' })
const diagnosticDimension = defineModel<string>('diagnosticDimension', { required: true })
const performanceMetric = defineModel<UsagePerformanceMetric>('performanceMetric', { required: true })
const costMetric = defineModel<UsageCostMetric>('costMetric', { required: true })
</script>

<template>
  <section
    class="mt-5 grid grid-cols-1 gap-3 xl:auto-rows-[minmax(24.25rem,auto)] xl:grid-cols-2"
    :aria-label="t('usage.insights.useStatisticalObservations')"
  >
    <UsageHealthCard
      :health="health.data?.data ?? null"
      :granularity="health.data?.granularity ?? ''"
      :loading="health.loading"
      :error="health.error"
    />
    <UsageDiagnosticCard
      v-model:dimension="diagnosticDimension"
      :diagnostics="diagnostics.data"
      :loading="diagnostics.loading"
      :error="diagnostics.error"
    />
    <UsagePerformanceCard
      v-model="performanceMetric"
      :performance="performance.data"
      :loading="performance.loading"
      :error="performance.error"
    />
    <UsageCostCard
      v-model="costMetric"
      :result="cost.data"
      :loading="cost.loading"
      :error="cost.error"
    />
  </section>
</template>
