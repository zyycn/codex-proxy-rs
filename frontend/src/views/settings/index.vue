<script setup lang="ts">
import { Save } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BasePageHeader from '@/components/base/BasePageHeader.vue'

import AdminApiKeyCard from './components/AdminApiKeyCard.vue'
import ModelAliasesCard from './components/ModelAliasesCard.vue'
import RotationStrategyCard from './components/RotationStrategyCard.vue'
import RuntimeSettingsCard from './components/RuntimeSettingsCard.vue'
import { useAdminApiKey } from './composables/useAdminApiKey'
import { useSettingsForm } from './composables/useSettingsForm'
import { rotationOptions } from './constants'

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
} = useAdminApiKey()
</script>

<template>
  <div class="w-full">
    <BasePageHeader title="系统设置" description="管理运行参数、调度策略、模型映射与外部访问配置">
      <template #actions>
        <BaseButton variant="primary" :loading="saving" :disabled="loading" @click="saveSettings">
          <template #icon>
            <Save class="size-4" />
          </template>
          {{ saving ? '保存中...' : '保存' }}
        </BaseButton>
      </template>
    </BasePageHeader>

    <div class="mt-5 grid w-full gap-5">
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
  </div>
</template>
