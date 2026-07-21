<script setup lang="ts">
import { RefreshCw } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'

import AccountOverviewCard from './components/AccountOverviewCard.vue'
import DashboardHeartbeat from './components/DashboardHeartbeat.vue'
import MetricCard from './components/MetricCard.vue'
import RequestHealthTimelineCard from './components/RequestHealthTimelineCard.vue'
import RequestTrendCard from './components/RequestTrendCard.vue'
import UsageRecordCard from './components/UsageRecordCard.vue'
import WireProfileCard from './components/WireProfileCard.vue'
import { useDashboard } from './composables/useDashboard'

const {
  loading,
  refreshing,
  trendLoading,
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
  <div class="w-full">
    <BasePageHeader title="系统概览">
      <template #description>
        <span>当日统计</span>
        <DashboardHeartbeat :updated-at="lastRefreshedAt" />
      </template>
      <template #actions>
        <BaseButton
          icon-only
          class="text-(--cp-normal)"
          size="md"
          label="刷新概览"
          :disabled="loading || refreshing"
          @click="refresh"
        >
          <RefreshCw :size="19" :class="loading || refreshing ? 'animate-spin' : undefined" />
        </BaseButton>
      </template>
    </BasePageHeader>

    <section
      class="mt-6 grid grid-cols-1 gap-4 md:grid-cols-2 2xl:grid-cols-4 2xl:gap-6"
      aria-label="核心指标"
    >
      <MetricCard v-for="metric in metrics" :key="metric.title" :metric="metric" />
    </section>

    <section
      class="mt-6 grid grid-cols-1 gap-6 2xl:grid-cols-[minmax(0,948fr)_minmax(0,608fr)] 2xl:gap-7"
    >
      <RequestTrendCard
        v-model:kind="activeTrendKind"
        :points="trendPoints"
        :summary="trendSummary"
        :loading="trendLoading"
        @trend-change="loadTrend"
      />
      <WireProfileCard :profiles="wireProfiles" />
    </section>

    <AccountOverviewCard
      :accounts="accountUsage"
      :pool="poolSummary"
      :capacity="capacityInfo"
      :rotation-strategy="rotationStrategy"
      class="mt-6"
    />

    <RequestHealthTimelineCard :timeline="healthTimeline" class="mt-6" />

    <UsageRecordCard :rows="usageRecords" class="mt-6" />
  </div>
</template>
