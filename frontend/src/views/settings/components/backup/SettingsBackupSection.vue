<script setup lang="ts">
import { computed, ref, watch } from 'vue'

import { useBackupRecords } from '../../composables/useBackupRecords'
import { useBackupSettings } from '../../composables/useBackupSettings'
import BackupRecordsCard from './BackupRecordsCard.vue'
import BackupScheduleCard from './BackupScheduleCard.vue'
import BackupStorageCard from './BackupStorageCard.vue'
import R2GuideModal from './R2GuideModal.vue'

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
  verified,
  storage,
  schedule,
  load: loadSettings,
  saveStorage,
  runTest,
  saveSchedule,
} = useBackupSettings()

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

const storageConfigured = computed(
  () =>
    Boolean(
      storage.value.endpoint.trim()
      && storage.value.region.trim()
      && storage.value.bucket.trim()
      && storage.value.accessKeyId.trim()
      && storage.value.secretAccessKey.trim(),
    ),
)

const storageReady = computed(
  () => loaded.value && storageConfigured.value && verified.value,
)

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
  <div class="grid w-full gap-5">
    <BackupStorageCard
      v-model:storage="storage"
      :disabled="busy || !loaded"
      :saving="savingStorage"
      :testing="testing"
      :verified="verified"
      @save="saveStorage()"
      @test="runTest()"
      @open-r2-guide="showR2Guide = true"
    />

    <BackupScheduleCard
      v-model:schedule="schedule"
      :disabled="busy || !loaded"
      :saving="savingSchedule"
      :storage-ready="storageReady"
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
