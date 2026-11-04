<script setup lang="ts">
import type { AccountGroupRef, AccountModelAccess } from '@/api'

import { ZFormItem, ZInput, ZSwitch, ZTextarea } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import AccountModelAccessField from './AccountModelAccessField.vue'
import AccountNameField from './AccountNameField.vue'
import AccountProxyField from './AccountProxyField.vue'

withDefaults(defineProps<{
  groups: AccountGroupRef[]
  groupsLoading: boolean
  disabled: boolean
  currentProxyId?: string | null
  endpoint?: string | null
  accountId?: string
  modelAccountId?: string
  preserveProxy?: boolean
  preserveModelAccess?: boolean
  showScheduling?: boolean
  namePlaceholder?: string
  notesPlaceholder?: string
}>(), { preserveProxy: true, showScheduling: true })

const { t } = useI18n({ useScope: 'global' })

const name = defineModel<string>('name')
const notes = defineModel<string>('notes')
const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const enabled = defineModel<boolean>('enabled')
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <div class="grid gap-4.5">
    <AccountModelAccessField v-model="modelAccess" form-prop="modelAccess" :account-id="modelAccountId ?? accountId" :disabled="disabled" :allow-preserve="preserveModelAccess" />
    <slot name="scheduling">
      <div v-if="showScheduling" class="flex min-h-6 items-center justify-between gap-3">
        <span class="text-z leading-none font-medium text-z-text-secondary">{{ t('accounts.scheduling.scheduling') }}</span>
        <ZSwitch
          v-model="enabled"
          :aria-label="t('accounts.scheduling.switchAccountSchedule')"
          :disabled="disabled"
        />
      </div>
    </slot>

    <div class="grid gap-4.5 sm:grid-cols-2">
      <ZFormItem prop="concurrencyLimit" :label="t('accounts.scheduling.concurrencyLimit')">
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
      <ZFormItem prop="weight" :show-message="Boolean(weight.trim())" :label="t('accounts.scheduling.weight')">
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
    <AccountProxyField v-model:mode="proxyMode" v-model:proxy-id="proxyId" form-prop="proxyId" :preserve="preserveProxy" :current-proxy-id="currentProxyId" :endpoint="endpoint" :account-id="accountId" :disabled="disabled" />
    <AccountNameField v-if="name !== undefined" v-model="name" :disabled="disabled" :placeholder="namePlaceholder" />
    <ZFormItem v-if="notes !== undefined" :label="t('common.fields.notes')">
      <ZTextarea v-model="notes" :rows="3" :maxlength="500" :placeholder="notesPlaceholder ?? t('accounts.editor.notesHelp')" :disabled="disabled" />
    </ZFormItem>
  </div>
</template>
