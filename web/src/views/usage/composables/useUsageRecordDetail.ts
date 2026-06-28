import { ref } from 'vue'

import { getUsageRecordDetail } from '@/api'
import { toast } from '@/components/base/BaseToast'

export function useUsageRecordDetail() {
  const showDetailModal = ref(false)
  const selectedUsageRecord = ref<any>(null)

  async function handleViewDetail(record: any) {
    try {
      const detail = await getUsageRecordDetail({ id: record.id })
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
