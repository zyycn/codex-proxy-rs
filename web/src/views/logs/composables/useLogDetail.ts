import { ref } from 'vue'

import { getLogDetail } from '@/api'
import { toast } from '@/components/base/BaseToast'

export function useLogDetail() {
  const showDetailModal = ref(false)
  const selectedLog = ref<any>(null)

  async function handleViewDetail(log: any) {
    try {
      const detail = await getLogDetail({ id: log.id })
      selectedLog.value = detail
      showDetailModal.value = true
    } catch (error: any) {
      toast.error(error.message || '加载详情失败')
    }
  }

  return {
    showDetailModal,
    selectedLog,
    handleViewDetail,
  }
}
