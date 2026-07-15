<script setup lang="ts">
import { RefreshCw } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'

import AccountOverviewCard from './components/AccountOverviewCard.vue'
import UsageRecordCard from './components/UsageRecordCard.vue'
import MetricCard from './components/MetricCard.vue'
import RequestHealthTimelineCard from './components/RequestHealthTimelineCard.vue'
import RequestTrendCard from './components/RequestTrendCard.vue'
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
  wireProfile,
  usageRecords,
  poolSummary,
  capacityInfo,
  rotationStrategy,
  refresh,
  loadTrend,
} = useDashboard()
</script>

<template>
  <div class="w-full">
    <header class="flex min-h-17 items-start justify-between gap-4">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          系统概览
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          当日统计 · 自动刷新 30s
        </p>
      </div>

      <BaseButton
        icon-only
        class="mt-0.5 text-(--cp-normal)"
        size="md"
        label="刷新概览"
        :disabled="loading || refreshing"
        @click="refresh"
      >
        <RefreshCw :size="19" :class="loading || refreshing ? 'animate-spin' : undefined" />
      </BaseButton>
    </header>

    <section
      class="mt-6 grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-4 xl:gap-6"
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
      <WireProfileCard :profile="wireProfile" />
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
