<script setup lang="ts">
import type { KeyUsageVersion } from '@/api/modules/key-usage'
import { BaseInput, BaseScrollbar } from '@codex-proxy/ui'
import { Search } from '@lucide/vue'
import { shallowRef } from 'vue'
import { getKeyUsageVersion } from '@/api/modules/key-usage'
import ApiKeyConfigModal from '@/components/ApiKeyConfigModal.vue'
import AppAboutModal from '@/components/AppAboutModal.vue'
import RequestHealthTimelineCard from '@/components/usage/RequestHealthTimelineCard.vue'
import KeyUsageBudget from './components/KeyUsageBudget.vue'
import KeyUsageHeader from './components/KeyUsageHeader.vue'
import KeyUsageRecords from './components/KeyUsageRecords.vue'
import KeyUsageSkeleton from './components/KeyUsageSkeleton.vue'
import KeyUsageSummary from './components/KeyUsageSummary.vue'
import KeyUsageTrend from './components/KeyUsageTrend.vue'
import { useKeyConfig } from './composables/useKeyConfig'
import { useKeyUsage } from './composables/useKeyUsage'

const { period, model, kind, refreshInterval, overview, overviewLoading, overviewError, refreshing, recordsStale, records, refresh, changePageSize, changePage } = useKeyUsage()
const { items, currentPage, pageSize, total, loading: recordsLoading, error: recordsError } = records
const { showConfig, configKey, configuring, apiBaseUrl, openConfig, copyConfig } = useKeyConfig()
const aboutOpen = shallowRef(false)
const version = shallowRef<KeyUsageVersion | null>(null)
const versionLoading = shallowRef(false)

async function openAbout() {
  aboutOpen.value = true
  if (version.value || versionLoading.value)
    return

  versionLoading.value = true
  try {
    version.value = await getKeyUsageVersion({ silent: true })
  }
  catch {
    // 版本不可用不影响查看项目信息，下次打开时重试。
  }
  finally {
    versionLoading.value = false
  }
}
</script>

<template>
  <main class="h-dvh overflow-hidden bg-cp-bg-layout text-cp-text">
    <BaseScrollbar>
      <div class="mx-auto flex min-h-full w-full max-w-480 flex-col gap-5 p-4 min-[961px]:p-6">
        <div class="flex flex-col gap-2">
          <KeyUsageHeader v-model:period="period" v-model:refresh-interval="refreshInterval" :name="overview?.key.name" :prefix="overview?.key.prefix" :refreshing="refreshing || overviewLoading" :configuring="configuring" @refresh="refresh" @configure="openConfig" @open-about="openAbout" />
          <div class="flex flex-wrap items-center justify-between gap-3">
            <span v-if="overview" class="text-cp-sm text-cp-text-tertiary">更新于 {{ overview.asOfDisplay }}</span>
            <BaseInput v-model="model" class="ml-auto w-60 max-w-full" placeholder="输入完整模型名称" aria-label="筛选统计和日志的模型" :maxlength="128">
              <template #prefix>
                <Search class="size-4" />
              </template>
            </BaseInput>
          </div>
        </div>
        <p v-if="overviewError" role="alert" class="m-0 rounded-cp-lg bg-cp-error-container px-4 py-3 text-cp-sm text-cp-error-text">
          {{ overviewError }}{{ overview ? '，暂时保留上次结果' : '，请点击顶部刷新重试' }}
        </p>
        <template v-if="overview">
          <KeyUsageSummary :summary="overview.summary" />
          <div class="grid min-w-0 gap-5 xl:grid-cols-[minmax(0,1.4fr)_minmax(400px,1fr)]">
            <KeyUsageTrend class="min-w-0" :points="overview.trend" />
            <KeyUsageBudget :budget="overview.key" />
          </div>
          <RequestHealthTimelineCard :timeline="overview.healthTimeline" />
        </template>
        <KeyUsageSkeleton v-else-if="overviewLoading" />
        <KeyUsageRecords v-model:kind="kind" :rows="items" :pagination="{ currentPage, pageSize, total }" :loading="recordsLoading" :error="recordsError" :stale="recordsStale" @page-change="changePage" @page-size-change="changePageSize" />
      </div>
    </BaseScrollbar>
    <ApiKeyConfigModal v-model="showConfig" title="密钥配置" :api-key="configKey" :api-base-url="apiBaseUrl" @copy="copyConfig" @after-leave="configKey = null" />
    <AppAboutModal v-model="aboutOpen" :version="version" />
  </main>
</template>
