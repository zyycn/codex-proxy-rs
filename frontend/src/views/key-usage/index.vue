<script setup lang="ts">
import { ZInput, ZScrollbar } from '@codex-proxy/ui'
import { Search } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import ApiKeyConfigModal from '@/components/ApiKeyConfigModal.vue'
import RequestHealthTimelineCard from '@/components/usage/RequestHealthTimelineCard.vue'
import KeyUsageBudget from './components/KeyUsageBudget.vue'
import KeyUsageHeader from './components/KeyUsageHeader.vue'
import KeyUsageRecords from './components/KeyUsageRecords.vue'
import KeyUsageSkeleton from './components/KeyUsageSkeleton.vue'
import KeyUsageSummary from './components/KeyUsageSummary.vue'
import KeyUsageTrend from './components/KeyUsageTrend.vue'
import { useKeyConfig } from './composables/useKeyConfig'
import { useKeyUsage } from './composables/useKeyUsage'

const { t } = useI18n({ useScope: 'global' })

const { period, model, kind, refreshInterval, overview, overviewLoading, refreshing, records, refresh, changePageSize, changePage } = useKeyUsage()
const { items, currentPage, pageSize, total, loading: recordsLoading } = records
const { showConfig, configKey, configuring, apiBaseUrl, openConfig, copyConfig } = useKeyConfig()
</script>

<template>
  <main class="h-dvh overflow-hidden bg-z-bg-layout text-z-text">
    <ZScrollbar>
      <div class="flex min-h-full w-full flex-col gap-5 p-4 min-[961px]:p-6">
        <div class="flex flex-col gap-2">
          <KeyUsageHeader v-model:period="period" v-model:refresh-interval="refreshInterval" :name="overview?.key.name" :prefix="overview?.key.prefix" :refreshing="refreshing || overviewLoading" :configuring="configuring" @refresh="refresh" @configure="openConfig" />
          <div class="flex flex-wrap items-center justify-between gap-3">
            <span v-if="overview" class="text-z-sm text-z-text-tertiary">{{ t('usage.page.updated', { asOfDisplay: overview.asOfDisplay }) }}</span>
            <ZInput id="key-usage-model" v-model="model" class="ml-auto w-full min-w-0 sm:w-75 sm:flex-none" :placeholder="t('usage.filters.modelPlaceholder')" :aria-label="t('usage.filters.modelPlaceholder')" :maxlength="128">
              <template #prefix>
                <Search class="size-4.5 text-z-text-tertiary" />
              </template>
            </ZInput>
          </div>
        </div>
        <template v-if="overview">
          <KeyUsageSummary :summary="overview.summary" />
          <div class="grid min-w-0 gap-5 xl:grid-cols-[minmax(0,1.4fr)_minmax(400px,1fr)]">
            <KeyUsageTrend class="min-w-0" :points="overview.trend" />
            <KeyUsageBudget :budget="overview.key" />
          </div>
          <RequestHealthTimelineCard :timeline="overview.healthTimeline" />
        </template>
        <KeyUsageSkeleton v-else-if="overviewLoading" />
        <KeyUsageRecords v-model:kind="kind" :rows="items" :pagination="{ currentPage, pageSize, total }" :loading="recordsLoading" @page-change="changePage" @page-size-change="changePageSize" />
      </div>
    </ZScrollbar>
    <ApiKeyConfigModal v-model="showConfig" :title="t('usage.header.keyConfiguration')" :api-key="configKey" :api-base-url="apiBaseUrl" @copy="copyConfig" @after-leave="configKey = null" />
  </main>
</template>
