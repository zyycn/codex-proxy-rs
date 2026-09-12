<script setup lang="ts">
import type { AccountRow } from '../../constants'
import type { Account } from '@/api'
import { ChartNoAxesCombined, Info, RefreshCw } from '@lucide/vue'
import { useNow } from '@vueuse/core'
import { computed, ref, toRef, useId, watch } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BasePopover from '@/components/base/BasePopover.vue'
import BaseSegmented from '@/components/base/BaseSegmented.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useAccountQuotaForecast } from '../../composables/useAccountQuotaForecast'
import AccountIdentityCell from '../AccountIdentityCell.vue'
import ForecastCapacity from './ForecastCapacity.vue'
import ForecastSkeleton from './Skeleton.vue'

const props = defineProps<{ account: AccountRow }>()
const emit = defineEmits<{ accountUpdated: [account: Account] }>()
const open = defineModel<boolean>({ default: false })
const period = ref('weekly')
const explanationOpen = ref(false)
const explanationId = useId()
const { report, loading, refreshing, error, load, refresh } = useAccountQuotaForecast(
  toRef(() => props.account.id),
  open,
  account => emit('accountUpdated', account),
)
const { now, pause, resume } = useNow({ interval: 30_000, controls: true })
const options = [
  { label: '周额度', value: 'weekly' },
  { label: '月额度', value: 'monthly' },
]
const forecast = computed(() => report.value?.forecasts.find(item => item.period === period.value))
const unavailableReason = computed(() => {
  if (forecast.value?.source && new Date(forecast.value.source.resetAt) <= now.value)
    return '额度窗口已过期，请刷新账号额度后重试。'
  return forecast.value?.unavailableReason ?? null
})

watch(open, (value) => {
  explanationOpen.value = false
  if (value) {
    period.value = 'weekly'
    resume()
  }
  else {
    pause()
  }
}, { immediate: true })

function handleExplanationKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && explanationOpen.value) {
    // 先关闭说明浮层，避免同一次按键也关闭其所属弹窗。
    event.preventDefault()
    event.stopPropagation()
    explanationOpen.value = false
  }
}
</script>

<template>
  <BaseModal
    v-model="open"
    title="额度预测"
    description="按当前用量结构，估算完整周期的容量"
    size="md"
    tone="info"
  >
    <template #icon>
      <ChartNoAxesCombined class="size-5 text-cp-primary-text" :stroke-width="1.75" />
    </template>

    <div class="grid gap-4">
      <div class="flex flex-wrap items-center justify-between gap-4">
        <AccountIdentityCell :account="account" show-plan title-mode="email">
          <template #meta>
            <ProviderIconGroup :provider="account.provider" size="sm" />
          </template>
        </AccountIdentityCell>
        <BaseSegmented v-model="period" label="预测周期" :options="options" class="w-48" />
      </div>

      <div aria-live="polite" :aria-busy="loading || refreshing">
        <ForecastSkeleton v-if="loading || refreshing" />
        <div v-else-if="error && !report" class="grid rounded-cp-card bg-cp-fill-tertiary/70 [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
          <BaseEmpty title="预测加载失败" description="暂时无法取得预测数据，请重新加载。" surface="none" class="min-h-80 content-center">
            <template #action>
              <BaseButton variant="secondary" @click="load">
                重新加载
              </BaseButton>
            </template>
          </BaseEmpty>
        </div>
        <template v-else-if="forecast">
          <div v-if="unavailableReason" class="grid rounded-cp-card bg-cp-fill-tertiary/70 [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
            <BaseEmpty title="暂时无法预测" :description="unavailableReason" :icon="ChartNoAxesCombined" surface="none" class="min-h-72 content-center" />
          </div>
          <ForecastCapacity v-else :forecast="forecast" />
        </template>
      </div>
    </div>

    <template #footer>
      <div class="mr-auto flex min-w-0 items-center gap-2">
        <BasePopover v-model="explanationOpen" trigger="hover-click" placement="top-start" :hover-delay="240">
          <template #trigger>
            <BaseButton
              variant="ghost"
              size="sm"
              :aria-expanded="explanationOpen"
              :aria-describedby="explanationOpen ? explanationId : undefined"
              @keydown="handleExplanationKeydown"
            >
              <template #icon>
                <Info class="size-3.5" />
              </template>
              预测说明
            </BaseButton>
          </template>
          <section :id="explanationId" role="tooltip" class="grid w-96 max-w-[calc(100vw-2rem)] gap-3 p-4 text-cp-xs leading-relaxed text-cp-text-secondary">
            <h4 class="m-0 font-heavy text-cp-text">
              仅供参考
            </h4>
            <p class="m-0">
              根据已记录用量估算，结果会随使用的模型和方式变化，并非官方承诺额度。
            </p>
            <p v-if="forecast?.lowSample" class="m-0">
              目前数据还较少，结果可能有较大波动。
            </p>
            <p v-if="forecast?.extrapolated" class="m-0">
              本页预测为折算值，剩余量仍按{{ forecast.source?.label ?? '当前周期' }}计算。
            </p>
            <p class="m-0">
              剩余量以数据更新时间为准；等价费用不是实际账单或账户余额。
            </p>
          </section>
        </BasePopover>
        <span v-if="forecast?.source?.observedAt" class="hidden text-cp-xs text-cp-text-tertiary sm:block">
          更新于 {{ forecast.source.observedAtDisplay }}
        </span>
      </div>
      <BaseButton variant="secondary" @click="open = false">
        关闭
      </BaseButton>
      <BaseButton variant="primary" :loading="refreshing" :disabled="loading" @click="refresh">
        <template #loading>
          <RefreshCw class="size-3.5 motion-safe:animate-spin" />
        </template>
        <template #icon>
          <RefreshCw class="size-3.5" />
        </template>
        刷新额度
      </BaseButton>
    </template>
  </BaseModal>
</template>
