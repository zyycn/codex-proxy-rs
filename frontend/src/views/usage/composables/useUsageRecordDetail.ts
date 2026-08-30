import type { UsageDisplayRecord, UsageViewModel } from '../utils/records'
import { shallowRef } from 'vue'
import { getUsageRecordDetail } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'
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
    catch (error: unknown) {
      toast.error(errorMessage(error, '加载详情失败'))
    }
  }

  return {
    showDetailModal,
    selectedUsageRecord,
    handleViewDetail,
  }
}
