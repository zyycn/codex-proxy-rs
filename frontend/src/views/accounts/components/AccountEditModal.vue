<script setup lang="ts">
import type { AccountRow } from '../constants'
import type { AccountGroup } from '@/api'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountPlanBadge from './AccountPlanBadge.vue'
import AccountSettingsFields from './AccountSettingsFields.vue'

defineProps<{
  account: AccountRow | null
  groups: AccountGroup[]
  groupsLoading: boolean
  saving: boolean
}>()

const emit = defineEmits<{
  save: []
}>()

const open = defineModel<boolean>({ required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyUrl = defineModel<string>('proxyUrl', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <BaseModal
    v-model="open"
    title="编辑账号"
    description="查看账号信息，并调整调度与所属分组。"
    size="md"
    :dismissible="!saving"
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
          <AccountPlanBadge :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" size="sm" />
          <ProviderIconGroup
            :provider="account.provider"
            :authentication-kind="account.authenticationKind"
          />
        </div>
      </div>

      <AccountSettingsFields
        v-model:enabled="enabled"
        v-model:concurrency-limit="concurrencyLimit"
        v-model:weight="weight"
        v-model:selected-group-ids="selectedGroupIds"
        v-model:proxy-mode="proxyMode"
        v-model:proxy-url="proxyUrl"
        :groups="groups"
        :groups-loading="groupsLoading"
        :disabled="saving"
        :endpoint="account.outboundProxyEndpoint"
      />
    </div>

    <template #footer>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="!account || groupsLoading"
        @click="emit('save')"
      >
        保存更改
      </BaseButton>
    </template>
  </BaseModal>
</template>
