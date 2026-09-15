<script setup lang="ts">
import type { RequestTrendKind } from '@/views/overview/model/display'
import { computed, shallowRef } from 'vue'

import OverviewContent from '@/views/overview/components/OverviewContent.vue'
import {
  dashboardSnapshotView,
  dashboardTrendView,
} from '@/views/overview/model/admin'
import { keyOverviewMetrics, keyOverviewTrend } from '@/views/overview/model/key'
import { keyUsageRecordColumns, usageRecordColumns } from '@/views/usage/model/columns'

import { themeDashboardSummary } from '../fixtures/dashboard'
import { themeKeyOverview } from '../fixtures/keyOverview'

const props = defineProps<{ isKey: boolean }>()

const adminSnapshot = dashboardSnapshotView(themeDashboardSummary)
const keySnapshot = {
  metrics: keyOverviewMetrics(themeKeyOverview),
  healthTimeline: themeKeyOverview.healthTimeline,
  accountOverview: null,
  wireProfiles: themeKeyOverview.wireProfiles,
  usageRecords: themeKeyOverview.usageRecords,
}
const snapshot = computed(() => props.isKey ? keySnapshot : adminSnapshot)
const trendKind = shallowRef<RequestTrendKind>('usage')
const trend = computed(() => props.isKey
  ? keyOverviewTrend(themeKeyOverview, trendKind.value)
  : dashboardTrendView({ ...themeDashboardSummary.trend, kind: trendKind.value }))
const overviewUsageColumns = usageRecordColumns.filter(column => column.key !== 'actions')
const usageColumns = computed(() => props.isKey ? keyUsageRecordColumns : overviewUsageColumns)
</script>

<template>
  <OverviewContent
    v-model:trend-kind="trendKind"
    last-refreshed-at="刚刚更新"
    :metrics="snapshot.metrics"
    :trend-points="trend.points"
    :trend-summary="trend.summary"
    :health-timeline="snapshot.healthTimeline"
    :account-overview="snapshot.accountOverview"
    :wire-profiles="snapshot.wireProfiles"
    :budget="isKey ? themeKeyOverview.budget : null"
    :limits="isKey ? themeKeyOverview.limits : null"
    :usage-records="snapshot.usageRecords"
    :usage-columns="usageColumns"
  />
</template>
