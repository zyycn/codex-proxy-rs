import type { UsageViewModel } from '@/api'
import { shallowRef } from 'vue'
import { getUsageRecordDetail, normalizeUsageRecord } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

export function useUsageRecordDetail() {
  const showDetailModal = shallowRef(false)
  const selectedUsageRecord = shallowRef<UsageViewModel | null>(null)

  async function handleViewDetail(record: UsageViewModel) {
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
