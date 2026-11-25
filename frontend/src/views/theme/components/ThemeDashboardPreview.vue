<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import DashboardOverview from '@/views/dashboard/components/DashboardOverview.vue'
import { themeDashboardSummary } from '../fixtures/dashboard'

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <DashboardOverview
    :trend-kind="themeDashboardSummary.trend.kind"
    :summary="themeDashboardSummary"
    :trend="themeDashboardSummary.trend"
    :last-refreshed-at="t('appearance.preview.dashboard.justUpdated')"
  />
</template>
