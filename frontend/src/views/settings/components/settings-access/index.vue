<script setup lang="ts">
import { ZConfirmDialog } from '@codex-proxy/ui'

import { onMounted } from 'vue'
import { useI18n } from 'vue-i18n'
import AdminApiKeyCard from './AdminApiKeyCard.vue'
import AdminPasswordCard from './AdminPasswordCard.vue'
import { useAdminApiKey } from './useAdminApiKey'

const { t } = useI18n({ useScope: 'global' })

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
  <div class="grid min-w-0 gap-5 @6xl/settings:grid-cols-2">
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

    <ZConfirmDialog
      v-model="showDeleteModal"
      :title="t('settings.access.deleteAdminApiKey')"
      :description="t('settings.access.deleteKeyDescription')"
      destructive
      :confirm-text="t('common.actions.delete')"
      :loading="deleting"
      @confirm="remove"
    >
      <p class="m-0">
        {{ t('settings.access.areYouSureYouWantToDelete') }}
      </p>
    </ZConfirmDialog>
  </div>
</template>
