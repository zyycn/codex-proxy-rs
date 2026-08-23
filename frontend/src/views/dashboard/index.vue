<script setup lang="ts">
import DashboardContent from './components/DashboardContent.vue'
import { useDashboard } from './composables/useDashboard'

const {
  loading,
  refreshing,
  activeTrendKind,
  metrics,
  trendPoints,
  trendSummary,
  healthTimeline,
  accountUsage,
  wireProfiles,
  usageRecords,
  poolSummary,
  capacityInfo,
  rotationStrategy,
  lastRefreshedAt,
  refresh,
  loadTrend,
} = useDashboard()
</script>

<template>
  <DashboardContent
    v-model:trend-kind="activeTrendKind"
    :loading="loading"
    :refreshing="refreshing"
    :last-refreshed-at="lastRefreshedAt"
    :metrics="metrics"
    :trend-points="trendPoints"
    :trend-summary="trendSummary"
    :health-timeline="healthTimeline"
    :account-usage="accountUsage"
    :wire-profiles="wireProfiles"
    :usage-records="usageRecords"
    :pool-summary="poolSummary"
    :capacity-info="capacityInfo"
    :rotation-strategy="rotationStrategy"
    @refresh="refresh"
    @trend-change="loadTrend"
  />
</template>
