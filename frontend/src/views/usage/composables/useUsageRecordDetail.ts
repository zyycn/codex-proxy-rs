import type { UsageListRecord, UsageRecordDetail } from '@/api'
import { shallowRef } from 'vue'
import { getUsageRecordDetail } from '@/api'

export function useUsageRecordDetail() {
  const showDetailModal = shallowRef(false)
  const selectedUsageRecord = shallowRef<UsageRecordDetail | null>(null)

  async function handleViewDetail(record: UsageListRecord) {
    try {
      const detail = await getUsageRecordDetail({ id: record.id })
      selectedUsageRecord.value = detail
      showDetailModal.value = true
    }
    catch {}
  }

  return {
    showDetailModal,
    selectedUsageRecord,
    handleViewDetail,
  }
}
