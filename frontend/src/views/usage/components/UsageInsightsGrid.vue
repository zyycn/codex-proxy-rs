<script setup lang="ts">
import UsageCostCard from './UsageCostCard.vue'
import UsageDiagnosticCard from './UsageDiagnosticCard.vue'
import UsageHealthCard from './UsageHealthCard.vue'
import UsagePerformanceCard from './UsagePerformanceCard.vue'

withDefaults(
  defineProps<{
    overview: any
    diagnostics: any
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const diagnosticDimension = defineModel('diagnosticDimension', {
  type: String,
  default: 'model',
})
</script>

<template>
  <section class="mt-5 grid grid-cols-1 gap-3 xl:grid-cols-2" aria-label="使用统计观测">
    <UsageHealthCard
      :health="overview.health"
      :granularity="overview.granularity"
      :loading="loading"
    />

    <UsageDiagnosticCard
      v-model:dimension="diagnosticDimension"
      :diagnostics="diagnostics"
      :loading="loading"
    />

    <UsagePerformanceCard :performance="overview.performance" :loading="loading" />

    <UsageCostCard :cost="overview.cost" :loading="loading" />
  </section>
</template>
