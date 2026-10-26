<script setup lang="ts">
import type { ApiKey } from '@/api'

import { ZButton, ZDialog, ZSegmented } from '@codex-proxy/ui'
import { ArrowRight } from '@lucide/vue'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { resetApiKeyBudget } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { notification as ZNotification } from '@/utils/notification'

const props = defineProps<{ apiKey: ApiKey | null }>()

const emit = defineEmits<{ reset: [] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })
const period = shallowRef('all')
const { loading, run } = useAsyncAction()
const periods = computed(() => ([
  {
    label: t('common.values.all'),
    value: 'all',
  },
  {
    label: t('keys.budgetReset.dailyQuota'),
    value: 'daily',
  },
  {
    label: t('keys.budgetReset.weeklyQuota'),
    value: 'weekly',
  },
]))
const windows = computed(() => props.apiKey
  ? [
      {
        value: 'daily',
        label: t('keys.budgetReset.dailyQuota'),
        used: props.apiKey.dailyUsedUsd,
      },
      {
        value: 'weekly',
        label: t('keys.budgetReset.weeklyQuota'),
        used: props.apiKey.weeklyUsedUsd,
      },
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
    ZNotification.success({
      message: t('keys.budgetReset.keyQuotaReset'),
    })
    emit('reset')
  })
}
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('keys.actions.resetUsedCredit')"
    :description="t('keys.budgetReset.description')"
    width="28rem"
    :show-close="!loading" :close-on-click-modal="!loading" :close-on-press-escape="!loading"
  >
    <div class="grid gap-5">
      <div class="grid min-w-0 gap-1.5">
        <span class="text-cp-xs text-cp-text-quaternary">{{ t('keys.budgetReset.targetKey') }}</span>
        <p class="m-0 wrap-break-word text-cp text-cp-text">
          {{ apiKey?.name }}
        </p>
      </div>

      <fieldset class="m-0 min-w-0 border-0 p-0" :disabled="loading">
        <legend class="mb-2.5 p-0 text-cp-sm text-cp-text-secondary">
          {{ t('keys.budgetReset.resetScope') }}
        </legend>
        <ZSegmented v-model="period" class="w-full" :aria-label="t('keys.budgetReset.resetScope')" :options="periods" :disabled="loading" />
      </fieldset>

      <div v-if="apiKey" class="rounded-cp bg-cp-fill-quaternary p-3.5" aria-live="polite" aria-atomic="true">
        <div class="mb-3 grid grid-cols-[3.5rem_minmax(0,1fr)_1rem_4.5rem] items-center gap-2 text-cp-xs text-cp-text-quaternary">
          <span class="col-span-2 text-right">{{ t('keys.budgetReset.currentlyUsed') }}</span>
          <span class="col-start-4 text-right">{{ t('keys.budgetReset.afterReset') }}</span>
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
              <span v-else class="text-right text-cp-xs text-cp-text-quaternary">{{ t('keys.budgetReset.remainUnchanged') }}</span>
            </dd>
          </div>
        </dl>
      </div>
    </div>

    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :loading="loading" :disabled="!apiKey" @click="resetBudget">
        {{ t('keys.budgetReset.confirmReset') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
