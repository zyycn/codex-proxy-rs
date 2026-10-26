<script setup lang="ts">
import type { Account } from '@/api'

import { ZButton, ZDialog, ZEmpty, ZIconButton, ZPopover, ZSegmented } from '@codex-proxy/ui'

import { ChartNoAxesCombined, CircleAlert, RefreshCw } from '@lucide/vue'
import { useIntervalFn, useNow } from '@vueuse/core'
import { computed, ref, toRef, useId, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { useAccountQuotaForecast } from '../../composables/useAccountQuotaForecast'
import AccountIdentityCell from '../AccountIdentityCell.vue'
import ForecastCapacity from './ForecastCapacity.vue'
import ForecastSkeleton from './Skeleton.vue'

const props = defineProps<{ account: Account }>()

const emit = defineEmits<{ accountUpdated: [account: Account] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })
const period = ref('weekly')
const explanationOpen = ref(false)
const explanationId = useId()
const { report, loading, refreshing, error, load, refresh } = useAccountQuotaForecast(
  toRef(() => props.account.id),
  open,
  account => emit('accountUpdated', account),
)
const { now, pause, resume } = useNow({
  controls: true,
  scheduler: callback => useIntervalFn(callback, 30_000),
})
const options = computed(() => report.value?.forecasts.map(item => ({
  label: item.period === 'weekly'
    ? t('accounts.quotaForecast.weeklyQuota')
    : t('accounts.quotaForecast.monthlyQuota'),
  value: item.period,
})) ?? [
  {
    label: t('accounts.quotaForecast.weeklyQuota'),
    value: 'weekly',
  },
  {
    label: t('accounts.quotaForecast.monthlyQuota'),
    value: 'monthly',
  },
])
const forecast = computed(() => report.value?.forecasts.find(item => item.period === period.value))
const unavailableReason = computed(() => {
  if (forecast.value?.source && new Date(forecast.value.source.resetAt) <= now.value)
    return t('accounts.quotaForecast.windowExpired')
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
  <ZDialog
    v-model="open"
    :title="t('accounts.quotaForecast.quotaForecast')"
    :description="t('accounts.quotaForecast.periodHelp')"
    width="36rem"
    type="info"
  >
    <template #icon>
      <ChartNoAxesCombined class="size-5 text-cp-primary-text" :stroke-width="1.75" />
    </template>

    <div class="grid grid-cols-1 gap-4">
      <div class="flex flex-wrap items-center justify-between gap-4">
        <AccountIdentityCell :account="account" show-plan title-mode="email" meta-position="secondary" meta-size="xs" class="max-w-full">
          <template #meta>
            <ProviderIconGroup :provider="account.provider" size="xs" />
          </template>
        </AccountIdentityCell>
        <ZSegmented v-model="period" :aria-label="t('accounts.quotaForecast.forecastPeriod')" :options="options" class="w-48" />
      </div>

      <div aria-live="polite" :aria-busy="loading || refreshing">
        <ForecastSkeleton v-if="loading || refreshing" />
        <div v-else-if="error && !report" class="grid rounded-cp-card bg-cp-fill-tertiary/70 [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
          <ZEmpty :title="t('accounts.quotaForecast.predictionLoadFailed')" :description="t('accounts.quotaForecast.loadFailed')" surface="none" class="min-h-80 content-center">
            <template #action>
              <ZButton @click="load">
                {{ t('accounts.quotaForecast.reload') }}
              </ZButton>
            </template>
          </ZEmpty>
        </div>
        <template v-else-if="forecast">
          <div v-if="unavailableReason" class="grid rounded-cp-card bg-cp-fill-tertiary/70 [html[data-theme=light]_&]:bg-cp-fill-quaternary/70">
            <ZEmpty :title="t('accounts.quotaForecast.unpredictableAtMoment')" :description="unavailableReason" :icon="ChartNoAxesCombined" surface="none" class="min-h-72 content-center" />
          </div>
          <ForecastCapacity v-else :forecast="forecast" />
        </template>
      </div>
    </div>

    <template #footer>
      <div class="mr-auto flex min-w-0 items-center gap-1">
        <span v-if="forecast?.source?.observedAt" class="hidden text-cp-xs text-cp-text-tertiary sm:block">
          {{ t('accounts.quotaForecast.updated', { observedAtDisplay: forecast.source.observedAtDisplay }) }}
        </span>
        <ZPopover v-model="explanationOpen" trigger="hover-click" placement="top-start" :hover-delay="240">
          <template #reference>
            <ZIconButton
              :aria-label="t('accounts.quotaForecast.forecastDescription')"
              size="small"
              :aria-expanded="explanationOpen"
              :aria-describedby="explanationOpen ? explanationId : undefined"
              @keydown="handleExplanationKeydown"
            >
              <CircleAlert class="size-3.5" aria-hidden="true" />
            </ZIconButton>
          </template>
          <section :id="explanationId" role="tooltip" class="grid w-96 max-w-[calc(100vw-2rem)] gap-3 p-4 text-cp-xs leading-relaxed text-cp-text-secondary">
            <h4 class="m-0 font-heavy text-cp-text">
              {{ t('accounts.quotaForecast.forReferenceOnly') }}
            </h4>
            <p class="m-0">
              {{ t('accounts.quotaForecast.methodDescription') }}
            </p>
            <p class="m-0">
              {{ t('accounts.quotaForecast.limitations') }}
            </p>
            <p v-if="forecast?.lowSample" class="m-0">
              {{ t('accounts.quotaForecast.limitedData') }}
            </p>
            <p v-if="forecast?.extrapolated" class="m-0">
              {{ t('accounts.quotaForecast.forecastCalculatedByOverDays', { value: forecast.source?.label ?? t('accounts.quotaForecast.currentPeriod'), targetDays: forecast.targetDays }) }}
            </p>
            <p class="m-0">
              {{ t('accounts.quotaForecast.equivalentCostHelp') }}
            </p>
          </section>
        </ZPopover>
      </div>
      <ZButton @click="open = false">
        {{ t('common.actions.close') }}
      </ZButton>
      <ZButton type="primary" :loading="refreshing" :disabled="loading" @click="refresh">
        <template #loading>
          <RefreshCw class="size-3.5 motion-safe:animate-spin" />
        </template>
        <template #icon>
          <RefreshCw class="size-3.5" />
        </template>
        {{ t('accounts.quotaForecast.refreshQuota') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
