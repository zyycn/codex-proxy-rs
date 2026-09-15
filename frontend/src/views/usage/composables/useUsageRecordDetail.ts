import type { UsageDisplayRecord, UsageViewModel } from '../utils/records'
import { shallowRef } from 'vue'
import { getUsageRecordDetail } from '@/api'
import { normalizeUsageRecord } from '../utils/records'

export function useUsageRecordDetail() {
  const showDetailModal = shallowRef(false)
  const selectedUsageRecord = shallowRef<UsageViewModel | null>(null)

  async function handleViewDetail(record: UsageDisplayRecord) {
    try {
      const detail = await getUsageRecordDetail({ id: record.id })
      selectedUsageRecord.value = normalizeUsageRecord(detail)
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
