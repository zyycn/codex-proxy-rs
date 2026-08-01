<script setup lang="ts">
import { Save } from '@lucide/vue'
import { computed, watch } from 'vue'
import { useRoute, useRouter } from 'vue-router'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'

import AdminApiKeyCard from './components/AdminApiKeyCard.vue'
import SettingsBackupSection from './components/backup/SettingsBackupSection.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import RuntimeSettingsCard from './components/RuntimeSettingsCard.vue'
import { useAdminApiKey } from './composables/useAdminApiKey'
import { useSettingsForm } from './composables/useSettingsForm'
import { rotationOptions } from './constants'

const route = useRoute()
const router = useRouter()

const section = computed(() => (route.params.section === 'backup' ? 'backup' : 'runtime'))

function switchSection(value: string): void {
  void router.push(value === 'backup' ? '/settings/backup' : '/settings')
}

const {
  loading,
  saving,
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

// 只在切到对应分区时才加载各自接口，避免打开备份页也请求运行设置数据。
watch(
  section,
  (value) => {
    if (value === 'runtime') {
      void loadSettings()
      void loadAdminApiKeyStatus()
    }
  },
  { immediate: true },
)
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="系统设置" description="管理运行参数、调度策略、模型映射与备份配置">
      <template #actions>
        <BaseButton
          v-if="section === 'runtime'"
          variant="primary"
          :loading="saving"
          :disabled="loading"
          @click="saveSettings"
        >
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存' }}
        </BaseButton>
      </template>
    </BasePageHeader>

    <div class="mt-4">
      <BaseSegmented
        :model-value="section"
        class="bg-(--cp-input-soft-bg)!"
        :options="[
          { label: '运行设置', value: 'runtime' },
          { label: '备份', value: 'backup' },
        ]"
        aria-label="设置分区"
        @update:model-value="switchSection"
      />
    </div>

    <div v-show="section === 'runtime'" class="mt-5 grid w-full gap-5">
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

      <RuntimeSettingsCard
        v-model:max-concurrent-per-account="maxConcurrentPerAccountValue"
        v-model:refresh-margin-seconds="refreshMarginSecondsValue"
        v-model:refresh-concurrency="refreshConcurrencyValue"
        v-model:request-interval-ms="requestIntervalMsValue"
      />

      <ModelAliasesCard
        :mappings="mappings"
        :loading="loading"
        :error="error"
        @add-mapping="addMapping"
        @update-mapping="updateMapping"
        @remove-mapping="removeMapping"
      />

      <RotationStrategyCard v-model="form.rotationStrategy" :options="rotationOptions" />

      <BaseConfirmModal
        v-model="showDeleteAdminKeyModal"
        title="删除管理员 API Key"
        description="删除后外部系统将无法继续使用该 Key 调用管理接口"
        variant="danger"
        confirm-text="确认删除"
        :loading="adminKeyDeleting"
        width="480px"
        @confirm="handleDeleteAdminApiKey"
      >
        <p class="m-0">
          确定要删除当前管理员 API Key 吗？此操作会立即生效
        </p>
      </BaseConfirmModal>
    </div>

    <div v-show="section === 'backup'" class="mt-5">
      <SettingsBackupSection :active="section === 'backup'" />
    </div>
  </div>
</template>
