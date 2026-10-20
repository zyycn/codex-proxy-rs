<script setup lang="ts">
import type { ApiKeyAccountForm } from '../utils/upstreamApiKey'

import type { Account, AccountGroupRef, AccountModelAccess } from '@/api'

import { ZButton, ZDialog, ZEmpty, ZFormItem, ZSegmented, ZTextarea } from '@codex-proxy/ui'
import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { isOpenAiApiKeyAccount, isOpenAiOAuthAccount } from '../utils/upstreamApiKey'
import AccountApiKeyFields from './AccountApiKeyFields.vue'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountSettingsFields from './AccountSettingsFields.vue'

defineProps<{
  account: Account | null
  groups: AccountGroupRef[]
  groupsLoading: boolean
  saving: boolean
  configurationLoading: boolean
  configurationReady: boolean
}>()

const emit = defineEmits<{
  save: []
}>()

const open = defineModel<boolean>({ required: true })
const apiKey = defineModel<ApiKeyAccountForm>('apiKey', { required: true })
const oauthTransport = defineModel<ApiKeyAccountForm['transport']>('oauthTransport', { required: true })
const transportOptions = [
  { label: 'WS', value: 'prefer_websocket' },
  { label: 'SSE', value: 'http' },
]
const notes = defineModel<string>('notes', { required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <ZDialog
    v-model="open"
    title="编辑账号"
    width="42rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
  >
    <div v-if="account" class="grid gap-5">
      <div
        class="flex flex-wrap items-center justify-between gap-4 rounded-cp bg-cp-fill-quaternary px-4 py-3.5"
      >
        <AccountIdentityCell
          class="min-w-0 flex-1"
          :account="account"
          size="lg"
        />
        <div class="flex shrink-0 items-center gap-3">
          <AccountPlanBadge :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" />
          <ProviderIconGroup
            :provider="account.provider"
            :authentication-kind="account.authenticationKind"
          />
        </div>
      </div>

      <ZFormItem v-if="isOpenAiOAuthAccount(account)" label="上游传输方式">
        <p v-if="configurationLoading" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
          正在读取上游设置…
        </p>
        <ZEmpty v-else-if="!configurationReady" title="传输方式读取失败" description="其他设置仍可保存" size="small" surface="none" role="alert" />
        <ZSegmented v-else v-model="oauthTransport" aria-label="上游传输方式" :options="transportOptions" :disabled="saving" />
      </ZFormItem>

      <section v-if="isOpenAiApiKeyAccount(account)" class="grid gap-4">
        <h3 class="m-0 text-cp font-heavy text-cp-text">
          上游连接
        </h3>
        <p v-if="configurationLoading" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
          正在读取上游设置…
        </p>
        <ZEmpty v-else-if="!configurationReady" title="上游设置读取失败" description="请关闭后重试" size="small" surface="none" role="alert" />
        <AccountApiKeyFields v-else v-model="apiKey" editing :disabled="saving" />
      </section>

      <AccountSettingsFields
        v-model:enabled="enabled"
        v-model:concurrency-limit="concurrencyLimit"
        v-model:weight="weight"
        v-model:model-access="modelAccess"
        v-model:selected-group-ids="selectedGroupIds"
        v-model:proxy-mode="proxyMode"
        v-model:proxy-id="proxyId"
        :groups="groups"
        :groups-loading="groupsLoading"
        :disabled="saving"
        :show-scheduling="false"
        :current-proxy-id="account.outboundProxyId"
        :endpoint="account.outboundProxyEndpoint"
        :account-id="account.id"
      />

      <ZFormItem label="备注">
        <ZTextarea
          v-model="notes"
          :rows="3"
          :maxlength="500"
          placeholder="最多 500 字，留空可清除备注"
          :disabled="saving"
        />
      </ZFormItem>
    </div>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        取消
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        :disabled="!account || groupsLoading || (isOpenAiApiKeyAccount(account) && !configurationReady)"
        @click="emit('save')"
      >
        保存更改
      </ZButton>
    </template>
  </ZDialog>
</template>
