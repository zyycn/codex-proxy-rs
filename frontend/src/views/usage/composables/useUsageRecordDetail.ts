import type { Ref } from 'vue'
import type { UsageTimeRangeParams } from './useUsageTimeRange'

import type { getUsageRecords } from '@/api'
import { shallowRef } from 'vue'
import { getUsageRecordDetail } from '@/api'
import { toast } from '@/components/base/BaseToast'
import { errorMessage } from '@/utils/async'

export function useUsageRecordDetail(options: {
  timeRangeParams?: Readonly<Ref<UsageTimeRangeParams>>
} = {}) {
  const showDetailModal = shallowRef(false)
  const selectedUsageRecord = shallowRef<Awaited<ReturnType<typeof getUsageRecordDetail>> | null>(
    null,
  )

  async function handleViewDetail(
    record: Awaited<ReturnType<typeof getUsageRecords>>['items'][number],
  ) {
    try {
      const detail = await getUsageRecordDetail({
        ...(options?.timeRangeParams?.value ?? {}),
        id: record.id,
      })
      selectedUsageRecord.value = detail
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
