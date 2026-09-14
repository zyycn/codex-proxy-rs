<script setup lang="ts">
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import OverviewContent from './components/OverviewContent.vue'

import { useOverview } from './composables/useOverview'

const {
  activeTrendKind,
  loading,
  refreshing,
  lastRefreshedAt,
  metrics,
  trendPoints,
  trendSummary,
  trendLoading,
  trendError,
  fatalError,
  healthTimeline,
  accountUsage,
  wireProfiles,
  budget,
  limits,
  usageRecords,
  poolSummary,
  capacityInfo,
  rotationStrategy,
  showAccountOverview,
  usageColumns,
  refresh,
  loadTrend,
} = useOverview()
</script>

<template>
  <BaseEmpty
    v-if="fatalError"
    class="min-h-80 place-content-center"
    :title="fatalError"
    description="请检查服务状态后重试"
  />
  <OverviewContent
    v-else
    v-model:trend-kind="activeTrendKind"
    :loading="loading"
    :refreshing="refreshing"
    :last-refreshed-at="lastRefreshedAt"
    :metrics="metrics"
    :trend-points="trendPoints"
    :trend-summary="trendSummary"
    :trend-loading="trendLoading"
    :trend-error="trendError"
    :health-timeline="healthTimeline"
    :account-usage="accountUsage"
    :wire-profiles="wireProfiles"
    :budget="budget"
    :limits="limits"
    :usage-records="usageRecords"
    :pool-summary="poolSummary"
    :capacity-info="capacityInfo"
    :rotation-strategy="rotationStrategy"
    :show-account-overview="showAccountOverview"
    :usage-columns="usageColumns"
    @refresh="refresh"
    @trend-change="loadTrend"
  />
</template>
