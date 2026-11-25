<script setup lang="ts">
import type { Account } from '@/api'

import { ZDialog } from '@codex-proxy/ui'
import { toRef } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountConcurrencyTable from './AccountConcurrencyTable.vue'
import { useAccountConcurrency } from './useAccountConcurrency'

const props = defineProps<{ account: Account | null }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const { data, entries, loading, error } = useAccountConcurrency(toRef(props, 'account'), open)
</script>

<template>
  <ZDialog v-model="open" :title="t('accounts.concurrency.title')" :description="t('accounts.concurrency.description')" width="72rem">
    <AccountConcurrencyTable
      :entries="entries"
      :loading="loading && !data" :aria-busy="loading"
      :empty-text="error ? t('accounts.concurrency.unavailable') : t('accounts.concurrency.empty')"
    />
  </ZDialog>
</template>
