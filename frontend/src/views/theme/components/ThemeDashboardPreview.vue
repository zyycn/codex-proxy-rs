<script setup lang="ts">
import type { DashboardTrendKind } from '@/api/modules/dashboard'
import { shallowRef } from 'vue'

import OverviewContent from '@/views/overview/components/OverviewContent.vue'
import {
  dashboardSnapshotView,
  dashboardTrendView,
} from '@/views/overview/model/admin'
import { usageRecordColumns } from '@/views/usage/model/columns'

import { themeDashboardSummary } from '../fixtures/dashboard'

const snapshot = dashboardSnapshotView(themeDashboardSummary)
const trend = dashboardTrendView(themeDashboardSummary.trend)
const trendKind = shallowRef<DashboardTrendKind>('usage')
const overviewUsageColumns = usageRecordColumns.filter(column => column.key !== 'actions')
</script>

<template>
  <OverviewContent
    v-model:trend-kind="trendKind"
    last-refreshed-at="刚刚更新"
    :metrics="snapshot.metrics"
    :trend-points="trend.points"
    :trend-summary="trend.summary"
    :health-timeline="snapshot.healthTimeline"
    :account-usage="snapshot.accountUsage"
    :wire-profiles="snapshot.wireProfiles"
    :usage-records="snapshot.usageRecords"
    :pool-summary="snapshot.poolSummary"
    :capacity-info="snapshot.capacityInfo"
    :rotation-strategy="snapshot.rotationStrategy"
    show-account-overview
    :usage-columns="overviewUsageColumns"
  />
</template>
