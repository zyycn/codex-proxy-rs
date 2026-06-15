<script setup lang="ts">
import AppTopbar from '@/layout/components/AppTopbar.vue'

import AccountOverviewCard from './components/AccountOverviewCard.vue'
import EventLogCard from './components/EventLogCard.vue'
import MetricCard from './components/MetricCard.vue'
import RequestTrendCard from './components/RequestTrendCard.vue'
import ServiceStatusCard from './components/ServiceStatusCard.vue'
import { useDashboard } from './composables/useDashboard'

const {
  metrics,
  trendPoints,
  trendSummary,
  accountUsage,
  serviceStatuses,
  eventLogs,
} = useDashboard()
</script>

<template>
  <div class="ml-7 w-[1584px] pt-[34px] pb-[60px]">
    <header class="flex h-[68px] items-start justify-between">
      <div>
        <h1 class="mt-0 text-[34px] leading-[1.15] font-[800] mb-0 text-[var(--cp-text-primary)]">
          系统概览
        </h1>
        <p class="mt-2.5 text-[15px] leading-[1.15] font-semibold mb-0 text-[var(--cp-text-secondary)]">
          生产环境 · 14:32 更新 · 自动刷新 30s
        </p>
      </div>

      <AppTopbar class="mt-0.5" />
    </header>

    <section class="mt-6 grid grid-cols-4 gap-6" aria-label="核心指标">
      <MetricCard v-for="metric in metrics" :key="metric.title" :metric="metric" />
    </section>

    <section class="mt-6 grid grid-cols-[948px_608px] gap-7">
      <RequestTrendCard :points="trendPoints" :summary="trendSummary" />
      <ServiceStatusCard :items="serviceStatuses" />
    </section>

    <AccountOverviewCard :accounts="accountUsage" class="mt-6" />

    <EventLogCard :rows="eventLogs" class="mt-6" />
  </div>
</template>
