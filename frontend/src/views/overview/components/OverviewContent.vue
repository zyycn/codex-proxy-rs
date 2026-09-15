<script setup lang="ts">
import type { ClientOverviewResponse, DashboardWireProfile } from '@/api'
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import type {
  AccountOverviewView,
  MetricCardView,
  RequestTrendKind,
  RequestTrendPoint,
  RequestTrendSummaryItem,
} from '@/views/overview/model/display'
import type { HealthTimeline } from '@/views/overview/model/health'
import type { UsageDisplayRecord } from '@/views/usage/model/records'
import OverviewAccountCard from './OverviewAccountCard.vue'
import OverviewHealthTimelineCard from './OverviewHealthTimelineCard.vue'
import OverviewSummary from './OverviewSummary.vue'
import OverviewUsageRecordCard from './OverviewUsageRecordCard.vue'

withDefaults(
  defineProps<{
    loading?: boolean
    refreshing?: boolean
    lastRefreshedAt?: string
    metrics: MetricCardView[]
    trendPoints: RequestTrendPoint[]
    trendSummary: RequestTrendSummaryItem[]
    trendLoading?: boolean
    trendError?: string
    healthTimeline: HealthTimeline
    accountOverview?: AccountOverviewView | null
    wireProfiles: DashboardWireProfile[]
    budget?: ClientOverviewResponse['budget'] | null
    limits?: ClientOverviewResponse['limits'] | null
    usageRecords: UsageDisplayRecord[]
    usageColumns: BaseTableColumn<UsageDisplayRecord>[]
  }>(),
  {
    loading: false,
    refreshing: false,
    lastRefreshedAt: '',
    trendLoading: false,
    trendError: '',
    accountOverview: null,
    budget: null,
    limits: null,
  },
)

const emit = defineEmits<{
  refresh: []
  trendChange: [kind: RequestTrendKind]
}>()

const trendKind = defineModel<RequestTrendKind>('trendKind', { required: true })
</script>

<template>
  <OverviewSummary
    v-model:trend-kind="trendKind"
    :loading="loading"
    :refreshing="refreshing"
    :last-refreshed-at="lastRefreshedAt"
    :metrics="metrics"
    :trend-points="trendPoints"
    :trend-summary="trendSummary"
    :trend-loading="trendLoading"
    :trend-error="trendError"
    :wire-profiles="wireProfiles"
    :budget="budget"
    :limits="limits"
    @refresh="emit('refresh')"
    @trend-change="emit('trendChange', $event)"
  >
    <OverviewAccountCard
      v-if="accountOverview"
      v-bind="accountOverview"
      class="mt-6"
    />

    <OverviewHealthTimelineCard :timeline="healthTimeline" class="mt-6" />

    <OverviewUsageRecordCard :columns="usageColumns" :rows="usageRecords" class="mt-6" />
  </OverviewSummary>
</template>
