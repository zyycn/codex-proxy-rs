<script setup lang="ts">
import type {
  getUsageRecordInsightsDiagnostics,
  UsageInsightsOverviewResponse,
} from '@/api'

import UsageCostCard from './UsageCostCard.vue'
import UsageDiagnosticCard from './UsageDiagnosticCard.vue'
import UsageHealthCard from './UsageHealthCard.vue'
import UsagePerformanceCard from './UsagePerformanceCard.vue'

withDefaults(
  defineProps<{
    overview: UsageInsightsOverviewResponse
    diagnostics: Awaited<ReturnType<typeof getUsageRecordInsightsDiagnostics>>
    loading?: boolean
    diagnosticDimensionOptions?: Array<{ label: string, value: string }>
  }>(),
  {
    loading: false,
    diagnosticDimensionOptions: undefined,
  },
)

const diagnosticDimension = defineModel('diagnosticDimension', {
  type: String,
  default: 'model',
})
</script>

<template>
  <section
    class="mt-5 grid grid-cols-1 gap-3 xl:auto-rows-97 xl:grid-cols-2"
    aria-label="使用统计观测"
  >
    <UsageHealthCard
      :health="overview.health"
      :granularity="overview.granularity"
      :loading="loading"
    />

    <UsageDiagnosticCard
      v-model:dimension="diagnosticDimension"
      :diagnostics="diagnostics"
      :loading="loading"
      :dimension-options="diagnosticDimensionOptions"
    />

    <UsagePerformanceCard
      :performance="overview.performance"
      :activity="overview.health.points"
      :loading="loading"
    />

    <UsageCostCard
      :cost="overview.cost"
      :activity="overview.health.points"
      :loading="loading"
    />
  </section>
</template>
