<script setup lang="ts">
import type { AccountGroup } from '@/api'
import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import AccountProxyField from './AccountProxyField.vue'

withDefaults(defineProps<{
  groups: AccountGroup[]
  groupsLoading: boolean
  disabled: boolean
  endpoint?: string | null
  accountId?: string
  preserveProxy?: boolean
  proxyError?: string
}>(), { preserveProxy: true })

const enabled = defineModel<boolean>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <div class="grid gap-5">
    <div class="flex min-h-6 items-center justify-between gap-3">
      <span class="text-cp leading-none font-medium text-cp-text-secondary">调度</span>
      <BaseSwitch
        v-model="enabled"
        label="切换账号调度"
        :disabled="disabled"
      />
    </div>

    <div class="grid gap-4 sm:grid-cols-2">
      <BaseFormItem label="并发限制">
        <BaseInput
          v-model="concurrencyLimit"
          aria-label="账号并发限制"
          type="number"
          min="1"
          max="4294967295"
          placeholder="留空使用默认值"
          :disabled="disabled"
        />
      </BaseFormItem>
      <BaseFormItem label="权重">
        <BaseInput
          v-model="weight"
          aria-label="账号调度权重"
          type="number"
          min="1"
          max="100"
          placeholder="越高越优先，最大 100"
          :disabled="disabled"
        />
      </BaseFormItem>
    </div>

    <BaseFormItem label="所属分组">
      <AccountGroupCheckboxGrid
        v-model="selectedGroupIds"
        :groups="groups"
        :loading="groupsLoading"
        :disabled="disabled"
      />
    </BaseFormItem>
    <AccountProxyField v-model:mode="proxyMode" v-model:proxy-id="proxyId" :preserve="preserveProxy" :error="proxyError" :endpoint="endpoint" :account-id="accountId" :disabled="disabled" />
  </div>
</template>
