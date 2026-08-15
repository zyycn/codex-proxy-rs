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
      storage.endpoint.trim()
      && storage.region.trim()
      && storage.bucket.trim()
      && storage.accessKeyId.trim()
      && storage.secretAccessKey.trim(),
    ),
)

const storageReady = computed(
  () => loaded.value && storageConfigured.value && verified.value,
)

watch(
  () => props.active,
  async (isActive) => {
    if (isActive) {
      await loadSettings()
      await loadRecords()
      startPolling()
    }
    else {
      stopPolling()
    }
  },
  { immediate: true },
)
</script>

<template>
  <div class="grid w-full gap-5">
    <BackupStorageCard
      v-model:storage="storage"
      :loading="settingsLoading"
      :saving="savingStorage"
      :testing="testing"
      :verified="verified"
      @save="saveStorage()"
      @test="runTest()"
      @open-r2-guide="showR2Guide = true"
    />

    <BackupScheduleCard
      v-model:schedule="schedule"
      :loading="settingsLoading"
      :saving="savingSchedule"
      :storage-ready="storageReady"
      @save="saveSchedule()"
    />

    <BackupRecordsCard
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
      @cancel-delete="deleteTarget = null"
    />

    <R2GuideModal v-model="showR2Guide" />
  </div>
</template>
