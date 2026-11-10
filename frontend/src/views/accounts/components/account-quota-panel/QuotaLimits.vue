<script setup lang="ts">
import type { Account } from '@/api'
import { ZButton, ZDialog, ZFormItem, ZInput } from '@codex-proxy/ui'
import { SlidersHorizontal } from '@lucide/vue'
import { computed, ref, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { batchUpdateAccounts, getAccountQuota } from '@/api'
import { quotaWindowLabel } from '@/components/account/account-usage-window/presenter'
import { useAsyncAction } from '@/composables/useAsyncAction'
import QuotaActionButton from './QuotaActionButton.vue'

const props = defineProps<{ account: Account }>()
const emit = defineEmits<{ accountUpdated: [account: Account] }>()
const { t } = useI18n({ useScope: 'global' })
const open = shallowRef(false)
const draft = ref<Record<string, string>>({})
const { loading, run } = useAsyncAction()
const rows = computed(() => {
  const labels = new Map(props.account.quota.windows.filter(w => w.accountWide).map(w => [
    w.key,
    quotaWindowLabel(w),
  ]))
  for (const key of Object.keys(props.account.quotaLimits)) {
    if (!labels.has(key))
      labels.set(key, key)
  }
  return Array.from(labels, ([key, label]) => ({ key, label }))
})
const invalid = computed(() => Object.values(draft.value).some(v => v !== '' && !/^(?:[1-9]\d?|100)$/.test(v)))

function edit() {
  draft.value = Object.fromEntries(Object.entries(props.account.quotaLimits).map(([key, value]) => [
    key,
    String(value),
  ]))
  open.value = true
}

async function save() {
  if (invalid.value)
    return
  const accountId = props.account.id
  const limits = Object.fromEntries(Object.entries(draft.value).filter(([, value]) => value !== '').map(([key, value]) => [
    key,
    Number(value),
  ]))
  await run(async () => {
    await batchUpdateAccounts({
      accountIds: [accountId],
      quotaLimits: limits,
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
  <QuotaActionButton :label="t('accounts.quotaLimits.title')" aria-haspopup="dialog" :pressed="open" @click="edit">
    <SlidersHorizontal class="size-3.5" />
  </QuotaActionButton>
  <ZDialog v-model="open" :title="t('accounts.quotaLimits.title')" :description="t('accounts.quotaLimits.help')" width="28rem" :close-on-click-modal="!loading" :close-on-press-escape="!loading" :show-close="!loading">
    <div class="grid gap-3">
      <p v-if="!rows.length" class="m-0 text-z-sm text-z-text-secondary">
        {{ t('accounts.quotaLimits.empty') }}
      </p>
      <ZFormItem v-for="row in rows" :key="row.key" :label="row.label">
        <ZInput v-model="draft[row.key]" type="number" min="1" max="100" step="1" :aria-label="row.label" :placeholder="t('accounts.quotaLimits.unlimited')" :disabled="loading">
          <template #suffix>
            %
          </template>
        </ZInput>
      </ZFormItem>
      <p v-if="invalid" class="m-0 text-z-xs text-z-error" role="alert">
        {{ t('accounts.quotaLimits.invalid') }}
      </p>
    </div>
    <template #footer>
      <ZButton :disabled="loading" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :disabled="!rows.length || invalid" :loading="loading" @click="save">
        {{ t('common.actions.save') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
