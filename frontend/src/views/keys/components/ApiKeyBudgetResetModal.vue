<script setup lang="ts">
import type { ApiKey } from '@/api'
import { BaseButton, BaseModal, BaseSegmented, toast } from '@codex-proxy/ui'
import { ArrowRight } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { resetApiKeyBudget } from '@/api'
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
const windows = computed(() => props.apiKey
  ? [
      { value: 'daily', label: '日额度', used: props.apiKey.dailyUsedUsd },
      { value: 'weekly', label: '周额度', used: props.apiKey.weeklyUsedUsd },
    ].map(window => ({ ...window, selected: period.value === 'all' || period.value === window.value }))
  : [])

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
  <BaseModal
    v-model="open"
    title="重置已用额度"
    description="清零所选周期的已用金额，下次使用时重新确定重置日期"
    size="sm"
    :dismissible="!loading"
  >
    <div class="grid gap-5">
      <div class="grid min-w-0 gap-1.5">
        <span class="text-cp-xs text-cp-text-quaternary">目标密钥</span>
        <p class="m-0 break-words text-cp text-cp-text">
          {{ apiKey?.name }}
        </p>
      </div>

      <fieldset class="m-0 min-w-0 border-0 p-0" :disabled="loading">
        <legend class="mb-2.5 p-0 text-cp-sm text-cp-text-secondary">
          重置范围
        </legend>
        <BaseSegmented v-model="period" class="w-full" label="重置范围" :options="periods" :disabled="loading" />
      </fieldset>

      <div v-if="apiKey" class="rounded-cp bg-cp-fill-quaternary p-3.5" aria-live="polite" aria-atomic="true">
        <div class="mb-3 grid grid-cols-[3.5rem_minmax(0,1fr)_1rem_4.5rem] items-center gap-2 text-cp-xs text-cp-text-quaternary">
          <span class="col-span-2 text-right">当前已用</span>
          <span class="col-start-4 text-right">重置后</span>
        </div>
        <dl class="m-0 grid gap-3">
          <div v-for="window in windows" :key="window.value" class="grid grid-cols-[3.5rem_minmax(0,1fr)_1rem_4.5rem] items-center gap-2">
            <dt class="text-cp-sm text-cp-text-secondary">
              {{ window.label }}
            </dt>
            <dd class="col-span-3 m-0 grid grid-cols-subgrid items-center">
              <span class="min-w-0 break-all text-right font-mono text-cp-sm text-cp-text tabular-nums">${{ window.used }}</span>
              <ArrowRight class="size-3.5 text-cp-text-quaternary" aria-hidden="true" />
              <span v-if="window.selected" class="text-right font-mono text-cp-sm text-cp-primary-text tabular-nums">$0</span>
              <span v-else class="text-right text-cp-xs text-cp-text-quaternary">保持不变</span>
            </dd>
          </div>
        </dl>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="secondary" :disabled="loading" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :loading="loading" :disabled="!apiKey" @click="resetBudget">
        确认重置
      </BaseButton>
    </template>
  </BaseModal>
</template>
