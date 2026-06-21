<script setup lang="ts">
import { RefreshCw } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BaseSpinner from '@/components/base/BaseSpinner.vue'
import AppTopbar from '@/layout/components/AppTopbar.vue'

import AccountOverviewCard from './components/AccountOverviewCard.vue'
import EventLogCard from './components/EventLogCard.vue'
import MetricCard from './components/MetricCard.vue'
import RequestTrendCard from './components/RequestTrendCard.vue'
import ServiceStatusCard from './components/ServiceStatusCard.vue'
import { useDashboard } from './composables/useDashboard'

const {
  loading,
  metrics,
  trendPoints,
  trendSummary,
  accountUsage,
  serviceStatuses,
  eventLogs,
  poolSummary,
  capacityInfo,
  rotationStrategy,
  refresh,
} = useDashboard()

</script>

<template>
  <div class="w-full min-w-295 p-7">
    <header class="flex h-17 items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-extrabold mb-0 text-(--cp-text-primary)">
          系统概览
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-(--cp-text-secondary)">
          生产环境 · 自动刷新 30s
        </p>
      </div>

      <div class="flex items-center gap-2">
        <BaseIconButton
          variant="ghost"
          size="md"
          title="刷新数据"
          :disabled="loading"
          @click="refresh"
        >
          <RefreshCw class="size-4.5" :class="{ 'animate-spin': loading }" />
        </BaseIconButton>
        <AppTopbar class="mt-0.5" />
      </div>
    </header>

    <BaseSpinner v-if="loading && metrics.length === 0" class="mt-20" />

    <template v-else>
      <section class="mt-6 grid grid-cols-4 gap-6" aria-label="核心指标">
        <MetricCard v-for="metric in metrics" :key="metric.title" :metric="metric" />
      </section>

      <section class="mt-6 grid grid-cols-[minmax(0,948fr)_minmax(0,608fr)] gap-7">
        <RequestTrendCard :points="trendPoints" :summary="trendSummary" />
        <ServiceStatusCard :items="serviceStatuses" />
      </section>

      <AccountOverviewCard
        :accounts="accountUsage"
        :pool="poolSummary"
        :capacity="capacityInfo"
        :rotation-strategy="rotationStrategy"
        class="mt-6"
      />

      <EventLogCard :rows="eventLogs" class="mt-6" />
    </template>
  </div>
</template>
