<script setup lang="ts">
import type { DashboardHealthTimeline, DashboardSummaryResponse, DashboardTrendKind, DashboardTrendResponse } from '@/api'
import { ZIconButton, ZPageHeader } from '@codex-proxy/ui'
import { RefreshCw } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import RequestHealthTimelineCard from '@/components/request-health-timeline-card/index.vue'
import AccountOverviewCard from './account-overview/index.vue'
import DashboardMetrics from './dashboard-metrics/index.vue'
import DashboardHeartbeat from './DashboardHeartbeat.vue'
import RequestTrendCard from './request-trend/index.vue'
import UsageRecordCard from './UsageRecordCard.vue'
import WireProfileCard from './wire-profile/index.vue'

defineProps<{
  summary: DashboardSummaryResponse | null
  trend: DashboardTrendResponse | null
  loading?: boolean
  refreshing?: boolean
  lastRefreshedAt?: string
  error?: string
  trendLoading?: boolean
  trendError?: string
}>()
const emit = defineEmits<{ refresh: [] }>()
const trendKind = defineModel<DashboardTrendKind>('trendKind', { required: true })
const { t } = useI18n({ useScope: 'global' })

const emptyHealthTimeline = computed<DashboardHealthTimeline>(() => ({
  title: t('dashboard.health.title'),
  description: t('dashboard.health.description'),
  reliabilityDisplay: '-',
  status: 'no_data',
  successRequests: 0,
  failedRequests: 0,
  cancelledRequests: 0,
  incompleteRequests: 0,
  callerErrorRequests: 0,
  points: [],
}))
</script>

<template>
  <div class="w-full">
    <ZPageHeader :title="t('dashboard.title')">
      <template #description>
        <span>{{ t('dashboard.today') }}</span>
        <DashboardHeartbeat :updated-at="lastRefreshedAt ?? summary?.asOfDisplay" />
      </template>
      <template #actions>
        <ZIconButton
          class="text-z-primary-text"
          size="default"
          :aria-label="t('dashboard.refresh.action')"
          :loading="loading || refreshing"
          :disabled="loading || refreshing"
          @click="emit('refresh')"
        >
          <template #loading>
            <RefreshCw class="animate-spin motion-reduce:animate-none" :size="19" />
          </template>
          <RefreshCw :size="19" />
        </ZIconButton>
      </template>
    </ZPageHeader>

    <template v-if="!error || summary">
      <DashboardMetrics :cards="summary?.cards" :points="summary?.trend.points" />

      <section
        class="mt-6 grid grid-cols-1 gap-6 2xl:grid-cols-[minmax(0,948fr)_minmax(0,608fr)] 2xl:gap-7"
      >
        <RequestTrendCard
          v-model:kind="trendKind"
          :trend="trend"
          :loading="trendLoading"
          :error="trendError"
        />
        <WireProfileCard :profiles="summary?.wireProfiles ?? []" />
      </section>

      <AccountOverviewCard
        :accounts="summary?.accountUsage ?? []"
        :pool="summary?.poolSummary"
        :capacity="summary?.capacityInfo"
        :rotation-strategy="summary?.rotationStrategy"
        class="mt-6"
      />

      <RequestHealthTimelineCard :timeline="summary?.healthTimeline ?? emptyHealthTimeline" class="mt-6" />

      <UsageRecordCard :rows="summary?.usageRecords ?? []" class="mt-6" />
    </template>
  </div>
</template>
