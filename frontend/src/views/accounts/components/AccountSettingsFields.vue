<script setup lang="ts">
import type { AccountGroupRef, AccountModelAccess } from '@/api'
import { ZFormItem, ZInput, ZSwitch } from '@codex-proxy/ui'
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

const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <div class="grid gap-5">
    <AccountModelAccessField v-model="modelAccess" :account-id="accountId" :disabled="disabled" :allow-preserve="preserveModelAccess" />
    <div v-if="showScheduling" class="flex min-h-6 items-center justify-between gap-3">
      <span class="text-cp leading-none font-medium text-cp-text-secondary">调度</span>
      <ZSwitch
        v-model="enabled"
        aria-label="切换账号调度"
        :disabled="disabled"
      />
    </div>

    <div class="grid gap-4 sm:grid-cols-2">
      <ZFormItem label="并发限制">
        <ZInput
          v-model="concurrencyLimit"
          aria-label="账号并发限制"
          type="number"
          min="1"
          max="4294967295"
          placeholder="留空使用默认值"
          :disabled="disabled"
        />
      </ZFormItem>
      <ZFormItem label="权重">
        <ZInput
          v-model="weight"
          aria-label="账号调度权重"
          type="number"
          min="1"
          max="100"
          placeholder="越高越优先，最大 100"
          :disabled="disabled"
        />
      </ZFormItem>
    </div>

    <ZFormItem label="所属分组">
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
