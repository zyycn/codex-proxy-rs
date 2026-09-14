<script setup lang="ts">
import type { ClientOverviewResponse, DashboardWireProfile } from '@/api'
import type {
  MetricCardView,
  RequestTrendKind,
  RequestTrendPoint,
  RequestTrendSummaryItem,
} from '@/views/overview/model/display'
import { RefreshCw } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'

import OverviewBudgetCard from './OverviewBudgetCard.vue'
import OverviewHeartbeat from './OverviewHeartbeat.vue'
import OverviewMetricCard from './OverviewMetricCard.vue'
import OverviewRequestTrendCard from './OverviewRequestTrendCard.vue'
import OverviewWireProfileCard from './OverviewWireProfileCard.vue'

withDefaults(defineProps<{
  loading?: boolean
  refreshing?: boolean
  lastRefreshedAt?: string
  metrics: MetricCardView[]
  trendPoints: RequestTrendPoint[]
  trendSummary: RequestTrendSummaryItem[]
  trendLoading?: boolean
  trendError?: string
  wireProfiles: DashboardWireProfile[]
  budget?: ClientOverviewResponse['budget'] | null
  limits?: ClientOverviewResponse['limits'] | null
}>(), {
  loading: false,
  refreshing: false,
  lastRefreshedAt: '',
  trendLoading: false,
  trendError: '',
  budget: null,
  limits: null,
})

const emit = defineEmits<{
  refresh: []
  trendChange: [kind: RequestTrendKind]
}>()

const trendKind = defineModel<RequestTrendKind>('trendKind', { required: true })
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="系统概览">
      <template #description>
        <span>当日统计</span>
        <OverviewHeartbeat :updated-at="lastRefreshedAt" />
      </template>
      <template #actions>
        <BaseIconButton
          class="text-cp-primary-text"
          size="md"
          label="刷新概览"
          :loading="loading || refreshing"
          :disabled="loading || refreshing"
          @click="emit('refresh')"
        >
          <template #loading>
            <RefreshCw class="animate-spin motion-reduce:animate-none" :size="19" />
          </template>
          <RefreshCw :size="19" />
        </BaseIconButton>
      </template>
    </BasePageHeader>

    <section class="mt-6 grid grid-cols-1 gap-4 md:grid-cols-2 2xl:grid-cols-4 2xl:gap-6" aria-label="核心指标">
      <OverviewMetricCard v-for="metric in metrics" :key="metric.title" :metric="metric" />
    </section>

    <section class="mt-6 grid grid-cols-1 gap-6 2xl:grid-cols-[minmax(0,948fr)_minmax(0,608fr)] 2xl:gap-7">
      <OverviewRequestTrendCard
        v-model:kind="trendKind"
        :points="trendPoints"
        :summary="trendSummary"
        :loading="trendLoading"
        :error="trendError"
        @trend-change="emit('trendChange', $event)"
      />
      <OverviewBudgetCard v-if="budget && limits" :budget="budget" :limits="limits" />
      <OverviewWireProfileCard v-else :profiles="wireProfiles" />
    </section>

    <slot />
  </div>
</template>
