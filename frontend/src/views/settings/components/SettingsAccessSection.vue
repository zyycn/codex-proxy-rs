<script setup lang="ts">
import { onMounted } from 'vue'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import { useAdminApiKey } from '../composables/useAdminApiKey'
import AdminApiKeyCard from './AdminApiKeyCard.vue'
import AdminPasswordCard from './AdminPasswordCard.vue'
import BlockDegradedTurnStateCard from './BlockDegradedTurnStateCard.vue'
import ClientVersionSettings from './client-version/index.vue'
import ResponseBodyLimitCard from './ResponseBodyLimitCard.vue'

defineProps<{
  disabled: boolean
  loading: boolean
  desktopError: string
  cliError: string
}>()

const minCodexDesktopVersion = defineModel<string>('minCodexDesktopVersion', { required: true })
const minCodexCliVersion = defineModel<string>('minCodexCliVersion', { required: true })
const responsesMaxDecompressedBodyMiB = defineModel<string>('responsesMaxDecompressedBodyMiB', { required: true })
const blockDegradedTurnState = defineModel<boolean>('blockDegradedTurnState', { required: true })

const {
  loading: adminKeyLoading,
  regenerating,
  deleting,
  showDeleteModal,
  generatedKey,
  status,
  regenerate,
  remove,
  copyGeneratedKey,
  loadStatus,
} = useAdminApiKey()

onMounted(loadStatus)
</script>

<template>
  <div class="grid min-w-0 gap-5">
    <AdminPasswordCard />

    <AdminApiKeyCard
      :status="status"
      :loading="adminKeyLoading"
      :regenerating="regenerating"
      :deleting="deleting"
      :generated-key="generatedKey"
      @regenerate="regenerate"
      @request-delete="showDeleteModal = true"
      @copy="copyGeneratedKey"
    />

    <fieldset :disabled="disabled" class="m-0 grid min-w-0 gap-5 border-0 p-0" aria-label="访问限制">
      <ClientVersionSettings
        v-model:min-codex-desktop-version="minCodexDesktopVersion"
        v-model:min-codex-cli-version="minCodexCliVersion"
        :loading="loading"
        :desktop-error="desktopError"
        :cli-error="cliError"
      />
      <ResponseBodyLimitCard v-model="responsesMaxDecompressedBodyMiB" />
      <BlockDegradedTurnStateCard v-model="blockDegradedTurnState" />
    </fieldset>

    <BaseConfirmModal
      v-model="showDeleteModal"
      title="删除管理员 API Key"
      description="删除后外部系统将无法继续使用该 Key 调用管理接口"
      destructive
      confirm-text="确认删除"
      :loading="deleting"
      @confirm="remove"
    >
      <p class="m-0">
        确定要删除当前管理员 API Key 吗？此操作会立即生效
      </p>
    </BaseConfirmModal>
  </div>
</template>
