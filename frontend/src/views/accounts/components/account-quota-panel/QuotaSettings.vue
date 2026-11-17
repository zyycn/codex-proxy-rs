<script setup lang="ts">
import type { FormRule } from '@codex-proxy/ui'
import type { Account, AccountQuotaBucketSettings } from '@/api'
import { ZButton, ZDialog, ZForm, ZFormItem, ZInput, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { SlidersHorizontal } from '@lucide/vue'
import { computed, ref, shallowRef, useId, useTemplateRef } from 'vue'
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
const formId = useId()
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
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
const limitRule = computed<FormRule>(() => ({
  validator: (_rule, value: string) => value === '' || /^(?:[1-9]\d?|100)$/.test(value),
  message: t('accounts.quotaSettings.invalid'),
}))
const modelRule = computed<FormRule>(() => ({
  required: true,
  validator: (_rule, value: string) => Boolean(value.trim()) && value.trim().length <= 128 && !/\p{Cc}/u.test(value),
  message: t('accounts.quotaSettings.invalidModel'),
}))

async function edit() {
  formRef.value?.clearValidate()
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
  if (loading.value || !draft.value.length || !await formRef.value?.validate().catch(() => false))
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
    <ZForm :id="formId" ref="formElement" :model="draft" :validate-on-rule-change="false" :show-message="false" class="gap-4" scroll-to-error focus-on-error novalidate @submit="save">
      <p v-if="!draft.length" class="m-0 text-z-sm text-z-text-secondary">
        {{ t('accounts.quotaSettings.empty') }}
      </p>
      <section v-for="(row, index) in draft" :key="row.key" class="grid gap-3 rounded-z bg-z-fill-quaternary p-3" :aria-label="row.label">
        <div class="flex items-center justify-between gap-3">
          <h3 class="m-0 min-w-0 wrap-anywhere text-z-sm font-heavy text-z-text">
            {{ row.label }}
          </h3>
          <ZSwitch
            v-model="row.warmup"
            size="small"
            inline-prompt
            class="w-12 [&:lang(en)]:w-18"
            width="100%"
            :active-text="t('accounts.quotaSettings.warmup')"
            :inactive-text="t('accounts.quotaSettings.warmup')"
            :aria-label="`${row.label} · ${t('accounts.quotaSettings.warmup')}`"
            :disabled="loading"
          />
        </div>
        <div class="grid gap-3 sm:grid-cols-2">
          <ZFormItem :label="t('accounts.quotaSettings.limit')" :prop="`${index}.limit`" :rules="limitRule">
            <ZInput v-model="row.limit" :validate-event="false" type="number" min="1" max="100" step="1" :aria-label="`${row.label} · ${t('accounts.quotaSettings.limit')}`" :placeholder="t('accounts.quotaSettings.unlimited')" :disabled="loading" @update:model-value="formRef?.clearValidate(`${index}.limit`)">
              <template #suffix>
                %
              </template>
            </ZInput>
          </ZFormItem>
          <ZFormItem v-if="row.warmup" :label="t('accounts.quotaSettings.model')" :prop="`${index}.model`" :rules="modelRule">
            <ZSelect v-model="row.model" :validate-event="false" class="w-full" :options="modelOptions" :aria-label="`${row.label} · ${t('accounts.quotaSettings.model')}`" :placeholder="t('accounts.quotaSettings.modelPlaceholder')" :empty-text="t('accounts.connectionTest.noModels')" :disabled="loading || loadingModels" @update:model-value="formRef?.clearValidate(`${index}.model`)" />
          </ZFormItem>
        </div>
      </section>
    </ZForm>
    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" native-type="submit" :form="formId" :disabled="!draft.length" :loading="loading">
        {{ t('common.actions.save') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
