<script setup lang="ts">
import type { ClientOverviewResponse } from '@/api'
import type { DashboardTrendKind } from '@/api/modules/dashboard'
import type { BaseTableColumn } from '@/components/base/BaseTable/columns'
import type { dashboardSnapshotView, dashboardTrendView } from '@/views/overview/model/admin'
import type { UsageDisplayRecord } from '@/views/usage/model/records'
import OverviewAccountCard from './OverviewAccountCard.vue'
import OverviewHealthTimelineCard from './OverviewHealthTimelineCard.vue'
import OverviewSummary from './OverviewSummary.vue'
import OverviewUsageRecordCard from './OverviewUsageRecordCard.vue'

type DashboardSnapshotView = ReturnType<typeof dashboardSnapshotView>
type DashboardTrendView = ReturnType<typeof dashboardTrendView>

withDefaults(
  defineProps<{
    loading?: boolean
    refreshing?: boolean
    lastRefreshedAt?: string
    metrics: DashboardSnapshotView['metrics']
    trendPoints: DashboardTrendView['points']
    trendSummary: DashboardTrendView['summary']
    trendLoading?: boolean
    trendError?: string
    healthTimeline: DashboardSnapshotView['healthTimeline']
    accountUsage: DashboardSnapshotView['accountUsage']
    wireProfiles: DashboardSnapshotView['wireProfiles']
    budget?: ClientOverviewResponse['budget'] | null
    limits?: ClientOverviewResponse['limits'] | null
    usageRecords: UsageDisplayRecord[]
    poolSummary: DashboardSnapshotView['poolSummary']
    capacityInfo: DashboardSnapshotView['capacityInfo']
    rotationStrategy: DashboardSnapshotView['rotationStrategy']
    showAccountOverview?: boolean
    usageColumns: BaseTableColumn<UsageDisplayRecord>[]
  }>(),
  {
    loading: false,
    refreshing: false,
    lastRefreshedAt: '',
    trendLoading: false,
    trendError: '',
    showAccountOverview: false,
    budget: null,
    limits: null,
  },
)

const emit = defineEmits<{
  refresh: []
  trendChange: [kind: DashboardTrendKind]
}>()

const trendKind = defineModel<DashboardTrendKind>('trendKind', { required: true })
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
      v-if="showAccountOverview"
      :accounts="accountUsage"
      :pool="poolSummary"
      :capacity="capacityInfo"
      :rotation-strategy="rotationStrategy"
      class="mt-6"
    />

    <OverviewHealthTimelineCard :timeline="healthTimeline" class="mt-6" />

    <OverviewUsageRecordCard :columns="usageColumns" :rows="usageRecords" class="mt-6" />
  </OverviewSummary>
</template>
