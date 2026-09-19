<script setup lang="ts">
import { Save, Undo2 } from '@lucide/vue'
import { computed, reactive, shallowRef, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import BaseSelect from '@/components/base/BaseSelect.vue'

import AccountAutoFreezeCard from './components/AccountAutoFreezeCard.vue'
import SettingsBackupSection from './components/backup/SettingsBackupSection.vue'
import ClientProfileCard from './components/ClientProfileCard.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import RequestLocationCard from './components/RequestLocationCard.vue'
import RequestQueueCard from './components/RequestQueueCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import RuntimeSettingsCard from './components/RuntimeSettingsCard.vue'
import SettingsAccessSection from './components/SettingsAccessSection.vue'
import TokenRefreshCard from './components/TokenRefreshCard.vue'
import { useSettingsForm } from './composables/useSettingsForm'
import { rotationOptions } from './constants'
import PricingSection from './pricing/index.vue'

const route = useRoute()
const router = useRouter()
const sectionOptions = [
  { label: '网关调度', value: 'runtime' },
  { label: '上游配置', value: 'upstream' },
  { label: '模型定价', value: 'pricing' },
  { label: '安全与访问', value: 'access' },
  { label: '数据备份', value: 'backup' },
]
const section = computed(() => {
  switch (route.name) {
    case 'settings-upstream': return 'upstream'
    case 'settings-access': return 'access'
    case 'settings-pricing': return 'pricing'
    case 'settings-backup': return 'backup'
    default: return 'runtime'
  }
})
const isBasicSection = computed(() => section.value !== 'pricing' && section.value !== 'backup')
const visited = reactive(new Set<string>())
const settingsVisited = shallowRef(false)

function switchSection(value: string): void {
  void router.push(value === 'runtime' ? '/settings' : `/settings/${value}`)
}

const {
  loading,
  saving,
  hasChanges,
  resetSettings,
  error,
  form,
  mappings,
  addMapping,
  updateMapping,
  removeMapping,
  refreshMarginSecondsValue,
  refreshConcurrencyValue,
  maxConcurrentPerAccountValue,
  requestIntervalMsValue,
  maxWaitingPerKeyValue,
  maxWaitingPerAccountValue,
  concurrencyWaitTimeoutSecondsValue,
  responsesMaxDecompressedBodyMiBValue,
  accountAutoFreezeThresholdValue,
  accountAutoFreezeWindowSecondsValue,
  accountAutoFreezeDurationSecondsValue,

  minCodexDesktopVersionError,
  minCodexCliVersionError,
  saveSettings,
  loadSettings,
} = useSettingsForm()

const disabled = computed(() => saving.value || loading.value || !!error.value)

watch(section, (value) => {
  visited.add(value)
  if (isBasicSection.value && !settingsVisited.value) {
    settingsVisited.value = true
    void loadSettings()
  }
}, { immediate: true })
</script>

<template>
  <div class="w-full" :class="section === 'pricing' ? 'flex h-[calc(100dvh-2rem)] flex-none! flex-col min-[961px]:h-[calc(100dvh-3rem)]' : undefined">
    <BasePageHeader title="系统设置" description="管理网关调度、上游配置、模型定价、安全访问与数据备份" />

    <div class="mt-4 flex min-h-cp-control shrink-0 flex-wrap items-center justify-between gap-3">
      <BaseSegmented
        :model-value="section"
        label="设置分区"
        class="hidden! bg-(--cp-input-bg)! sm:inline-grid!"
        :options="sectionOptions"
        @update:model-value="switchSection"
      />
      <BaseSelect
        :model-value="section"
        aria-label="设置分区"
        class="w-full sm:hidden"
        :options="sectionOptions"
        @update:model-value="switchSection"
      />
      <div v-if="isBasicSection || hasChanges" class="ml-auto flex items-center justify-end gap-2">
        <span v-if="hasChanges" class="mr-1 size-1.5 shrink-0 rounded-full bg-cp-warning" aria-hidden="true" />
        <BaseIconButton v-if="hasChanges" label="撤销全部基础设置更改" variant="filled" :disabled="saving || loading" @click="resetSettings">
          <Undo2 class="size-4" />
        </BaseIconButton>
        <BaseButton variant="primary" :loading="saving" :disabled="loading || !hasChanges || !!error" @click="saveSettings">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存基础设置' }}
        </BaseButton>
      </div>
    </div>

    <div v-if="settingsVisited" v-show="isBasicSection" class="mt-5 grid w-full gap-5">
      <div v-if="error" role="alert" class="flex flex-wrap items-center justify-between gap-3 rounded-cp-card bg-cp-error-container p-5 text-cp-error-on-container">
        <p class="m-0 text-cp">
          设置加载失败：{{ error }}
        </p>
        <BaseButton :loading="loading" @click="loadSettings()">
          重新加载
        </BaseButton>
      </div>

      <SettingsAccessSection
        v-if="visited.has('access')"
        v-show="section === 'access'"
        v-model:min-codex-desktop-version="form.minCodexDesktopVersion"
        v-model:min-codex-cli-version="form.minCodexCliVersion"
        v-model:responses-max-decompressed-body-mi-b="responsesMaxDecompressedBodyMiBValue"
        :disabled="disabled"
        :loading="loading"
        :desktop-error="minCodexDesktopVersionError"
        :cli-error="minCodexCliVersionError"
      />

      <fieldset v-show="section !== 'access'" :disabled="disabled" class="m-0 grid min-w-0 gap-5 border-0 p-0" aria-label="基础设置">
        <template v-if="section === 'runtime'">
          <RuntimeSettingsCard
            v-model:max-concurrent-per-account="maxConcurrentPerAccountValue"
            v-model:request-interval-ms="requestIntervalMsValue"
          />
          <RotationStrategyCard v-model="form.rotationStrategy" :options="rotationOptions" />
          <RequestQueueCard
            v-model:max-waiting-per-key="maxWaitingPerKeyValue"
            v-model:max-waiting-per-account="maxWaitingPerAccountValue"
            v-model:concurrency-wait-timeout-seconds="concurrencyWaitTimeoutSecondsValue"
          />
          <AccountAutoFreezeCard
            v-model:enabled="form.accountAutoFreezeEnabled"
            v-model:threshold="accountAutoFreezeThresholdValue"
            v-model:window-seconds="accountAutoFreezeWindowSecondsValue"
            v-model:duration-seconds="accountAutoFreezeDurationSecondsValue"
            v-model:probe-enabled="form.accountAutoFreezeProbeEnabled"
            v-model:probe-model="form.accountAutoFreezeProbeModel"
            v-model:adaptive-concurrency="form.accountAutoFreezeAdaptiveConcurrency"
          />
        </template>

        <div v-if="visited.has('upstream')" v-show="section === 'upstream'" class="grid min-w-0 gap-5">
          <TokenRefreshCard v-model:refresh-margin-seconds="refreshMarginSecondsValue" v-model:refresh-concurrency="refreshConcurrencyValue" />
          <ClientProfileCard
            v-model:openai="form.openaiClientProfile"
            v-model:xai="form.xaiClientProfile"
            :active="section === 'upstream'"
            :disabled="disabled"
          />
          <RequestLocationCard v-model="form.requestLocation" v-model:enabled="form.requestLocationEnabled" :disabled="disabled" />
          <ModelAliasesCard
            :mappings="mappings"
            :loading="loading"
            @add-mapping="addMapping"
            @update-mapping="updateMapping"
            @remove-mapping="removeMapping"
          />
        </div>
      </fieldset>
    </div>

    <SettingsBackupSection v-if="visited.has('backup')" v-show="section === 'backup'" class="mt-5" :active="section === 'backup'" />
    <PricingSection v-if="visited.has('pricing')" v-show="section === 'pricing'" class="mt-5 min-h-0 flex-1" />
  </div>
</template>
