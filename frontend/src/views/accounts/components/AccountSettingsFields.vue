<script setup lang="ts">
import type { AccountGroupRef, AccountModelAccess } from '@/api'

import { ZFormItem, ZInput, ZSwitch } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import AccountModelAccessField from './AccountModelAccessField.vue'
import AccountProxyField from './AccountProxyField.vue'

withDefaults(defineProps<{
  groups: AccountGroupRef[]
  groupsLoading: boolean
  disabled: boolean
  currentProxyId?: string | null
  endpoint?: string | null
  accountId?: string
  preserveProxy?: boolean
  preserveModelAccess?: boolean
  showScheduling?: boolean
  proxyError?: string
}>(), { preserveProxy: true, showScheduling: true })

const { t } = useI18n({ useScope: 'global' })

const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const enabled = defineModel<boolean>('enabled')
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <div class="grid gap-5">
    <AccountModelAccessField v-model="modelAccess" :account-id="accountId" :disabled="disabled" :allow-preserve="preserveModelAccess" />
    <slot name="scheduling">
      <div v-if="showScheduling" class="flex min-h-6 items-center justify-between gap-3">
        <span class="text-cp leading-none font-medium text-cp-text-secondary">{{ t('accounts.scheduling.scheduling') }}</span>
        <ZSwitch
          v-model="enabled"
          :aria-label="t('accounts.scheduling.switchAccountSchedule')"
          :disabled="disabled"
        />
      </div>
    </slot>

    <div class="grid gap-4 sm:grid-cols-2">
      <ZFormItem :label="t('accounts.scheduling.concurrencyLimit')">
        <ZInput
          v-model="concurrencyLimit"
          :aria-label="t('accounts.scheduling.accountConcurrencyLimit')"
          type="number"
          min="1"
          max="4294967295"
          :placeholder="t('accounts.scheduling.defaultHint')"
          :disabled="disabled"
        />
      </ZFormItem>
      <ZFormItem :label="t('accounts.scheduling.weight')">
        <ZInput
          v-model="weight"
          :aria-label="t('accounts.scheduling.accountSchedulingWeight')"
          type="number"
          min="1"
          max="100"
          :placeholder="t('accounts.scheduling.weightHelp')"
          :disabled="disabled"
        />
      </ZFormItem>
    </div>

    <ZFormItem :label="t('accounts.scheduling.groupLabel')">
      <AccountGroupCheckboxGrid
        v-model="selectedGroupIds"
        :groups="groups"
        :loading="groupsLoading"
        :disabled="disabled"
      />
    </ZFormItem>
    <AccountProxyField v-model:mode="proxyMode" v-model:proxy-id="proxyId" :preserve="preserveProxy" :error="proxyError" :current-proxy-id="currentProxyId" :endpoint="endpoint" :account-id="accountId" :disabled="disabled" />
  </div>
</template>
