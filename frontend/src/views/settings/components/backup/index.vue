<script setup lang="ts">
import { ref, watch } from 'vue'

import BackupRecordsCard from './BackupRecordsCard.vue'
import BackupScheduleCard from './BackupScheduleCard.vue'
import BackupStorageCard from './BackupStorageCard.vue'
import R2GuideModal from './R2GuideModal.vue'
import { useBackupRecords } from './useBackupRecords'
import { useBackupSettings } from './useBackupSettings'

const props = defineProps<{
  active: boolean
}>()

const {
  loading: settingsLoading,
  busy,
  savingStorage,
  testing,
  savingSchedule,
  loaded,
  hasChanges,
  verified,
  storageDirty,
  secretConfigured,
  storage,
  schedule,
  load: loadSettings,
  saveStorage,
  runTest,
  saveSchedule,
} = useBackupSettings()

defineExpose({ hasChanges })

const {
  records,
  page,
  pageSize,
  total,
  loading: recordsLoading,
  error,
  activeBackup,
  creating,
  refreshing,
  deleting,
  deleteTarget,
  showDelete,
  downloadStates,
  load: loadRecords,
  refresh,
  changePage,
  changePageSize,
  create,
  downloadBackup,
  requestDelete,
  confirmDelete,
  startPolling,
  stopPolling,
} = useBackupRecords()

const showR2Guide = ref(false)

// 首次进入备份页签时加载配置，后续切换只刷新记录，不覆盖草稿。
watch(
  () => props.active,
  async (isActive, _previous, onCleanup) => {
    let cancelled = false
    onCleanup(() => {
      cancelled = true
      stopPolling()
    })
    if (isActive) {
      if (!loaded.value && !settingsLoading.value)
        await loadSettings()
      if (cancelled)
        return
      await loadRecords()
      if (!cancelled)
        startPolling()
    }
  },
  { immediate: true },
)
</script>

<template>
  <div class="grid w-full min-w-0 gap-5">
    <BackupStorageCard
      v-model:storage="storage"
      :disabled="busy || !loaded"
      :saving="savingStorage"
      :testing="testing"
      :verified="verified"
      :storage-dirty="storageDirty"
      :secret-configured="secretConfigured"
      @save="saveStorage()"
      @test="runTest()"
      @open-r2-guide="showR2Guide = true"
    />

    <BackupScheduleCard
      v-model:schedule="schedule"
      :disabled="busy || !loaded"
      :saving="savingSchedule"
      :storage-ready="verified"
      @save="saveSchedule()"
    />

    <BackupRecordsCard
      v-model:delete-open="showDelete"
      :records="records"
      :page="page"
      :page-size="pageSize"
      :total="total"
      :loading="recordsLoading"
      :error="error"
      :active-backup="activeBackup"
      :creating="creating"
      :refreshing="refreshing"
      :deleting="deleting"
      :delete-target="deleteTarget"
      :download-states="downloadStates"
      @page-change="changePage($event)"
      @page-size-change="changePageSize($event)"
      @create="create()"
      @refresh="refresh()"
      @download="downloadBackup($event)"
      @request-delete="requestDelete($event)"
      @confirm-delete="confirmDelete()"
    />

    <R2GuideModal v-model="showR2Guide" />
  </div>
</template>
