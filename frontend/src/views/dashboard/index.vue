<script setup lang="ts">
import DashboardOverview from './components/DashboardOverview.vue'
import { useDashboard } from './composables/useDashboard'

const {
  summary,
  trend,
  activeTrendKind,
  loading,
  refreshing,
  summaryError,
  trendLoading,
  trendError,
  refresh,
  loadTrend,
} = useDashboard()
</script>

<template>
  <DashboardOverview
    :summary="summary"
    :trend="trend"
    :trend-kind="activeTrendKind"
    :loading="loading"
    :refreshing="refreshing"
    :error="summaryError"
    :trend-loading="trendLoading"
    :trend-error="trendError"
    @refresh="refresh"
    @update:trend-kind="loadTrend"
  />
</template>
