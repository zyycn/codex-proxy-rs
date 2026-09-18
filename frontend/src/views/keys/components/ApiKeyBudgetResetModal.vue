<script setup lang="ts">
import type { ApiKey } from '@/api'
import { shallowRef, watch } from 'vue'
import { resetApiKeyBudget } from '@/api'
import BaseConfirmModal from '@/components/base/BaseConfirmModal.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import { toast } from '@/components/base/BaseToast'
import { useAsyncAction } from '@/composables/useAsyncAction'

const props = defineProps<{ apiKey: ApiKey | null }>()
const emit = defineEmits<{ reset: [] }>()
const open = defineModel<boolean>({ default: false })
const period = shallowRef('all')
const { loading, run } = useAsyncAction()
const periods = [
  { label: '全部', value: 'all' },
  { label: '日额度', value: 'daily' },
  { label: '周额度', value: 'weekly' },
]

watch(open, (value) => {
  if (value)
    period.value = 'all'
})

async function resetBudget() {
  const key = props.apiKey
  const selectedPeriod = period.value
  if (!key || (selectedPeriod !== 'all' && selectedPeriod !== 'daily' && selectedPeriod !== 'weekly'))
    return
  await run(async () => {
    await resetApiKeyBudget({ id: key.id, period: selectedPeriod })
    open.value = false
    toast.success('已重置密钥额度')
    emit('reset')
  })
}
</script>

<template>
  <BaseConfirmModal
    v-model="open"
    title="重置已用额度"
    description="选择要清零的日额度、周额度或全部额度"
    confirm-text="确认重置"
    :loading="loading"
    :confirm-disabled="!apiKey"
    @confirm="resetBudget"
  >
    <div class="space-y-4">
      <p class="m-0 break-words text-cp-text">
        {{ apiKey?.name }}
      </p>
      <BaseSegmented v-model="period" class="w-full" label="重置范围" :options="periods" :disabled="loading" />
      <p class="m-0 text-cp leading-relaxed text-cp-text-secondary">
        限额上限、原重置时间和历史使用记录保持不变，正在进行的请求会在完成后计入额度
      </p>
    </div>
  </BaseConfirmModal>
</template>
