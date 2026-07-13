import { shallowRef } from 'vue'

import { getUsageRecordDetail } from '@/api'
import { toast } from '@/components/base/BaseToast'

export function useUsageRecordDetail(options: any = {}) {
  const showDetailModal = shallowRef(false)
  const selectedUsageRecord = shallowRef<any>(null)

  async function handleViewDetail(record: any) {
    try {
      const detail = await getUsageRecordDetail({
        ...(options?.timeRangeParams?.value ?? {}),
        id: record.id,
      })
      selectedUsageRecord.value = detail
      showDetailModal.value = true
    } catch (error: any) {
      toast.error(error.message || '加载详情失败')
    }
  }

  return {
    showDetailModal,
    selectedUsageRecord,
    handleViewDetail,
  }
}
