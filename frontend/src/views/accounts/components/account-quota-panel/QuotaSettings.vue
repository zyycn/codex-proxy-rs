<script setup lang="ts">
import type { Account, AccountQuotaBucketSettings } from '@/api'
import { ZButton, ZDialog, ZFormItem, ZInput, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { SlidersHorizontal } from '@lucide/vue'
import { computed, ref, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { batchUpdateAccounts, getAccountModels, getAccountQuota } from '@/api'
import { quotaWindowLabel } from '@/components/account/account-usage-window/presenter'
import { useAsyncAction } from '@/composables/useAsyncAction'
import QuotaActionButton from './QuotaActionButton.vue'

interface BucketDraft {
  key: string
  label: string
  limit: string
  warmup: boolean
  model: string
}

const props = defineProps<{ account: Account }>()
const emit = defineEmits<{ accountUpdated: [account: Account] }>()
const { t } = useI18n({ useScope: 'global' })
const open = shallowRef(false)
const draft = ref<BucketDraft[]>([])
const { loading, run } = useAsyncAction()
const { loading: loadingModels, run: loadModels } = useAsyncAction()
const models = shallowRef<Array<{ id: string, label: string }>>([])
const modelOptions = computed(() => {
  const options = new Map(models.value.map(model => [model.id, model.label]))
  for (const row of draft.value) {
    if (row.model && !options.has(row.model))
      options.set(row.model, row.model)
  }
  return Array.from(options, ([value, label]) => ({ value, label }))
})
const invalidLimit = computed(() => draft.value.some(row => row.limit !== '' && !/^(?:[1-9]\d?|100)$/.test(row.limit)))
const invalidModel = computed(() => draft.value.some(row => row.warmup && (!row.model.trim() || row.model.trim().length > 128 || /\p{Cc}/u.test(row.model))))
const invalid = computed(() => invalidLimit.value || invalidModel.value)

async function edit() {
  const windows = new Map(props.account.quota.windows.filter(window => window.accountWide).map(window => [window.key, window]))
  const keys = new Set([
    ...windows.keys(),
    ...Object.keys(props.account.quotaSettings),
  ])
  draft.value = Array.from(keys, (key) => {
    const window = windows.get(key)
    const settings = props.account.quotaSettings[key]
    return {
      key,
      label: window
        ? quotaWindowLabel(window)
        : key,
      limit: settings?.limitPercent == null
        ? ''
        : String(settings.limitPercent),
      warmup: settings?.warmup != null,
      model: settings?.warmup?.model ?? '',
    }
  })
  open.value = true
  const accountId = props.account.id
  await loadModels(async () => {
    const result = await getAccountModels({ accountId })
    if (props.account.id === accountId)
      models.value = result.models
  })
}

async function save() {
  if (invalid.value)
    return
  const accountId = props.account.id
  const settings: Record<string, AccountQuotaBucketSettings> = Object.fromEntries(draft.value
    .filter(row => row.limit !== '' || row.warmup)
    .map(row => [
      row.key,
      {
        limitPercent: row.limit === ''
          ? null
          : Number(row.limit),
        warmup: row.warmup
          ? {
              model: row.model.trim(),
            }
          : null,
      },
    ]))
  await run(async () => {
    await batchUpdateAccounts({
      accountIds: [accountId],
      quotaSettings: settings,
    })
    const { account } = await getAccountQuota({ accountId })
    if (props.account.id === accountId) {
      emit('accountUpdated', account)
      open.value = false
    }
  })
}
</script>

<template>
  <QuotaActionButton :label="t('accounts.quotaSettings.title')" aria-haspopup="dialog" :pressed="open" @click="edit">
    <SlidersHorizontal class="size-3.5" />
  </QuotaActionButton>
  <ZDialog v-model="open" :title="t('accounts.quotaSettings.title')" :description="t('accounts.quotaSettings.subtitle')" width="32rem" :close-on-click-modal="!loading" :close-on-press-escape="!loading" :show-close="!loading">
    <div class="grid gap-4">
      <p v-if="!draft.length" class="m-0 text-z-sm text-z-text-secondary">
        {{ t('accounts.quotaSettings.empty') }}
      </p>
      <section v-for="row in draft" :key="row.key" class="relative grid gap-3 rounded-z bg-z-fill-quaternary p-3" :aria-label="row.label">
        <h3 class="m-0 pr-32 text-z-sm font-heavy text-z-text">
          {{ row.label }}
        </h3>
        <ZSwitch v-model="row.warmup" class="absolute! top-2 right-3" :label="t('accounts.quotaSettings.warmup')" :aria-label="`${row.label} · ${t('accounts.quotaSettings.warmup')}`" :disabled="loading" />
        <div class="grid gap-3 sm:grid-cols-2">
          <ZFormItem :label="t('accounts.quotaSettings.limit')">
            <ZInput v-model="row.limit" type="number" min="1" max="100" step="1" :aria-label="`${row.label} · ${t('accounts.quotaSettings.limit')}`" :placeholder="t('accounts.quotaSettings.unlimited')" :disabled="loading">
              <template #suffix>
                %
              </template>
            </ZInput>
          </ZFormItem>
          <ZFormItem v-if="row.warmup" :label="t('accounts.quotaSettings.model')" required>
            <ZSelect v-model="row.model" class="w-full" :options="modelOptions" :aria-label="`${row.label} · ${t('accounts.quotaSettings.model')}`" :placeholder="t('accounts.quotaSettings.modelPlaceholder')" :empty-text="t('accounts.connectionTest.noModels')" :disabled="loading || loadingModels" />
          </ZFormItem>
        </div>
      </section>
      <p v-if="invalid" class="m-0 text-z-xs text-z-error" role="alert">
        {{ invalidLimit ? t('accounts.quotaSettings.invalid') : t('accounts.quotaSettings.invalidModel') }}
      </p>
    </div>
    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :disabled="!draft.length || invalid" :loading="loading" @click="save">
        {{ t('common.actions.save') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
