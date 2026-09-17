<script setup lang="ts">
import { Save, Undo2 } from '@lucide/vue'
import { computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseIconButton from '@/components/base/BaseIconButton.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

import AccountAutoFreezeCard from './components/AccountAutoFreezeCard.vue'
import AdminApiKeyCard from './components/AdminApiKeyCard.vue'
import SettingsBackupSection from './components/backup/SettingsBackupSection.vue'
import ClientVersionSettings from './components/client-version/index.vue'
import FastPolicyCard from './components/FastPolicyCard.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import RequestLocationCard from './components/RequestLocationCard.vue'
import RequestQueueCard from './components/RequestQueueCard.vue'
import ResponseBodyLimitCard from './components/ResponseBodyLimitCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import RuntimeSettingsCard from './components/RuntimeSettingsCard.vue'
import TokenRefreshCard from './components/TokenRefreshCard.vue'
import { useAdminApiKey } from './composables/useAdminApiKey'
import { useSettingsForm } from './composables/useSettingsForm'
import { rotationOptions } from './constants'

const route = useRoute()
const router = useRouter()
const section = computed(() => route.name === 'settings-backup' ? 'backup' : 'runtime')

function switchSection(value: string): void {
  void router.push(value === 'backup' ? '/settings/backup' : '/settings')
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

const {
  loading: adminKeyLoading,
  regenerating: adminKeyRegenerating,
  deleting: adminKeyDeleting,
  showDeleteModal: showDeleteAdminKeyModal,
  generatedKey: generatedAdminApiKey,
  status: adminApiKeyStatus,
  regenerate: handleRegenerateAdminApiKey,
  remove: handleDeleteAdminApiKey,
  copyGeneratedKey: copyAdminApiKey,
  loadStatus: loadAdminApiKeyStatus,
} = useAdminApiKey()

onMounted(() => {
  void loadSettings()
  void loadAdminApiKeyStatus()
})
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="系统设置" description="管理运行参数、管理员凭据与备份配置" />

    <div class="mt-4 flex min-h-cp-control flex-wrap items-center justify-between gap-3">
      <BaseSegmented
        :model-value="section"
        label="设置分区"
        class="bg-(--cp-input-bg)!"
        :options="[
          { label: '运行设置', value: 'runtime' },
          { label: '备份', value: 'backup' },
        ]"
        @update:model-value="switchSection"
      />
      <div v-if="section === 'runtime'" class="flex flex-wrap items-center justify-end gap-2">
        <BaseIconButton v-if="hasChanges" label="撤销更改" :disabled="saving || loading" @click="resetSettings">
          <Undo2 class="size-4" />
        </BaseIconButton>
        <span class="relative inline-flex">
          <BaseButton variant="primary" :loading="saving" :disabled="loading || !hasChanges || !!error" @click="saveSettings">
            <template #icon>
              <Save class="size-4" />
            </template>
            {{ saving ? '保存中...' : '保存' }}
          </BaseButton>
          <span v-if="hasChanges" class="pointer-events-none absolute top-1 right-1 size-1.5 rounded-full bg-cp-warning ring-2 ring-cp-bg-layout" aria-hidden="true" />
          <span class="sr-only" role="status">{{ hasChanges ? '有未保存更改' : '' }}</span>
        </span>
      </div>
    </div>

    <!-- 切换页签只隐藏内容，保留运行设置和备份配置的未保存草稿。 -->
    <div v-show="section === 'runtime'" class="mt-5 grid w-full gap-5">
      <div v-if="error" role="alert" class="flex flex-wrap items-center justify-between gap-3 rounded-cp-card bg-cp-error-container p-5 text-cp-error-on-container">
        <p class="m-0 text-cp">
          设置加载失败：{{ error }}
        </p>
        <BaseButton :loading="loading" @click="loadSettings()">
          重新加载
        </BaseButton>
      </div>
      <div v-else-if="loading" role="status" class="rounded-cp-card bg-cp-bg-container p-6 text-cp text-cp-text-secondary shadow-cp-card">
        正在加载设置…
      </div>

      <AdminApiKeyCard
        :status="adminApiKeyStatus"
        :loading="adminKeyLoading"
        :regenerating="adminKeyRegenerating"
        :deleting="adminKeyDeleting"
        :generated-key="generatedAdminApiKey"
        @regenerate="handleRegenerateAdminApiKey"
        @request-delete="showDeleteAdminKeyModal = true"
        @copy="copyAdminApiKey"
      />

      <fieldset :disabled="saving || loading || !!error" class="m-0 grid min-w-0 gap-5 border-0 p-0" aria-label="运行设置">
        <RuntimeSettingsCard
          v-model:max-concurrent-per-account="maxConcurrentPerAccountValue"
          v-model:request-interval-ms="requestIntervalMsValue"
        />
        <TokenRefreshCard v-model:refresh-margin-seconds="refreshMarginSecondsValue" v-model:refresh-concurrency="refreshConcurrencyValue" />
        <ResponseBodyLimitCard v-model="responsesMaxDecompressedBodyMiBValue" />
        <RequestQueueCard
          v-model:max-waiting-per-key="maxWaitingPerKeyValue"
          v-model:max-waiting-per-account="maxWaitingPerAccountValue"
          v-model:concurrency-wait-timeout-seconds="concurrencyWaitTimeoutSecondsValue"
        />
        <FastPolicyCard v-model="form.disableFast" :disabled="saving || loading || !!error" />

        <RequestLocationCard v-model="form.requestLocation" v-model:enabled="form.requestLocationEnabled" :disabled="saving || loading || !!error" />
        <AccountAutoFreezeCard
          v-model:enabled="form.accountAutoFreezeEnabled"
          v-model:threshold="accountAutoFreezeThresholdValue"
          v-model:window-seconds="accountAutoFreezeWindowSecondsValue"
          v-model:duration-seconds="accountAutoFreezeDurationSecondsValue"
          v-model:probe-enabled="form.accountAutoFreezeProbeEnabled"
          v-model:probe-model="form.accountAutoFreezeProbeModel"
          v-model:adaptive-concurrency="form.accountAutoFreezeAdaptiveConcurrency"
        />

        <ClientVersionSettings
          v-model:min-codex-desktop-version="form.minCodexDesktopVersion"
          v-model:min-codex-cli-version="form.minCodexCliVersion"
          :loading="loading"
          :desktop-error="minCodexDesktopVersionError"
          :cli-error="minCodexCliVersionError"
        />

        <ModelAliasesCard
          :mappings="mappings"
          :loading="loading"
          @add-mapping="addMapping"
          @update-mapping="updateMapping"
          @remove-mapping="removeMapping"
        />
        <RotationStrategyCard v-model="form.rotationStrategy" :options="rotationOptions" />
      </fieldset>
    </div>

    <SettingsBackupSection v-show="section === 'backup'" class="mt-5" :active="section === 'backup'" />

    <BaseConfirmModal
      v-model="showDeleteAdminKeyModal"
      title="删除管理员 API Key"
      description="删除后外部系统将无法继续使用该 Key 调用管理接口"
      destructive
      confirm-text="确认删除"
      :loading="adminKeyDeleting"
      @confirm="handleDeleteAdminApiKey"
    >
      <p class="m-0">
        确定要删除当前管理员 API Key 吗？此操作会立即生效
      </p>
    </BaseConfirmModal>
  </div>
</template>
