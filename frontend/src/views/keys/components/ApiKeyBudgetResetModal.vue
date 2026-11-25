<script setup lang="ts">
import type { ApiKey } from '@/api'

import { ZButton, ZDialog, ZSegmented } from '@codex-proxy/ui'
import { ArrowRight } from '@lucide/vue'
import { computed, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { resetApiKeyBudget } from '@/api'
import { useAsyncAction } from '@/composables/useAsyncAction'
import { notification as ZNotification } from '@/plugins/notification'

import ApiKeyBudgetAlignment from './ApiKeyBudgetAlignment.vue'

const props = defineProps<{ apiKey: ApiKey | null }>()

const emit = defineEmits<{ reset: [] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ default: false })
const period = shallowRef('all')
const mode = shallowRef('reset')
const alignment = useTemplateRef<InstanceType<typeof ApiKeyBudgetAlignment>>('alignment')
const aligning = shallowRef(false)
const { loading: resetting, run } = useAsyncAction()
const loading = computed(() => resetting.value || aligning.value)
const modes = computed(() => [
  {
    value: 'reset',
    label: t('keys.actions.resetUsedCredit'),
  },
  {
    value: 'align',
    label: t('keys.alignment.title'),
  },
])
function finishAlignment() {
  open.value = false
  emit('reset')
}
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
  if (value) {
    mode.value = 'reset'
    period.value = 'all'
    aligning.value = false
  }
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
    :title="t('keys.budgetReset.title')"
    :description="mode === 'reset' ? t('keys.budgetReset.description') : t('keys.alignment.description')"
    width="28rem"
    :show-close="!loading" :close-on-click-modal="!loading" :close-on-press-escape="!loading"
  >
    <div class="grid gap-5">
      <div class="grid min-w-0 gap-1.5">
        <span class="text-z-xs text-z-text-quaternary">{{ t('keys.budgetReset.targetKey') }}</span>
        <p class="m-0 wrap-break-word text-z text-z-text">
          {{ apiKey?.name }}
        </p>
      </div>

      <ZSegmented v-model="mode" class="w-full" :options="modes" :aria-label="t('keys.budgetReset.title')" :disabled="loading" />
      <ApiKeyBudgetAlignment v-if="mode === 'align' && apiKey && open" ref="alignment" :api-key="apiKey" @busy="aligning = $event" @saved="finishAlignment" />

      <fieldset v-if="mode === 'reset'" class="m-0 min-w-0 border-0 p-0" :disabled="loading">
        <legend class="mb-2.5 p-0 text-z-sm text-z-text-secondary">
          {{ t('keys.budgetReset.resetScope') }}
        </legend>
        <ZSegmented v-model="period" class="w-full" :aria-label="t('keys.budgetReset.resetScope')" :options="periods" :disabled="loading" />
      </fieldset>

      <div v-if="apiKey && mode === 'reset'" class="rounded-z bg-z-fill-quaternary p-3.5" aria-live="polite" aria-atomic="true">
        <div class="mb-3 grid grid-cols-[3.5rem_minmax(0,1fr)_1rem_4.5rem] items-center gap-2 text-z-xs text-z-text-quaternary">
          <span class="col-span-2 text-right">{{ t('keys.budgetReset.currentlyUsed') }}</span>
          <span class="col-start-4 text-right">{{ t('keys.budgetReset.afterReset') }}</span>
        </div>
        <dl class="m-0 grid gap-3">
          <div v-for="window in windows" :key="window.value" class="grid grid-cols-[3.5rem_minmax(0,1fr)_1rem_4.5rem] items-center gap-2">
            <dt class="text-z-sm text-z-text-secondary">
              {{ window.label }}
            </dt>
            <dd class="col-span-3 m-0 grid grid-cols-subgrid items-center">
              <span class="min-w-0 break-all text-right font-mono text-z-sm text-z-text tabular-nums">${{ window.used }}</span>
              <ArrowRight class="size-3.5 text-z-text-quaternary" aria-hidden="true" />
              <span v-if="window.selected" class="text-right font-mono text-z-sm text-z-primary-text tabular-nums">$0</span>
              <span v-else class="text-right text-z-xs text-z-text-quaternary">{{ t('keys.budgetReset.remainUnchanged') }}</span>
            </dd>
          </div>
        </dl>
      </div>
    </div>

    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton v-if="mode === 'align'" type="primary" :loading="aligning" :disabled="!alignment?.canSubmit" @click="alignment?.submit()">
        {{ t('keys.alignment.confirm') }}
      </ZButton>
      <ZButton v-else type="primary" :loading="resetting" :disabled="!apiKey" @click="resetBudget">
        {{ t('keys.budgetReset.confirmReset') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
