<script setup lang="ts">
import type { AccountRow } from '../constants'
import type { AccountGroup } from '@/api'

import AccountGroupCheckboxGrid from '@/components/AccountGroupCheckboxGrid.vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BaseFormItem from '@/components/base/BaseForm/FormItem.vue'
import BaseInput from '@/components/base/BaseInput.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import BaseSwitch from '@/components/base/BaseSwitch.vue'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import AccountIdentityCell from './AccountIdentityCell.vue'
import AccountPlanBadge from './AccountPlanBadge.vue'

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
          <AccountPlanBadge :plan-type="account.planType" size="sm" />
          <ProviderIconGroup
            :provider="account.provider"
            :authentication-kind="account.authenticationKind"
          />
        </div>
      </div>

      <div class="flex min-h-6 items-center justify-between gap-3">
        <span class="text-cp leading-none font-medium text-cp-text-secondary">调度</span>
        <BaseSwitch
          v-model="enabled"
          label="切换账号调度"
          :disabled="saving"
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
            :disabled="saving"
          />
        </BaseFormItem>
        <BaseFormItem label="权重">
          <BaseInput
            v-model="weight"
            aria-label="所选账号调度权重"
            type="number"
            min="1"
            max="100"
            placeholder="越高越优先，最大 100"
            :disabled="saving"
          />
        </BaseFormItem>
      </div>

      <BaseFormItem label="所属分组">
        <AccountGroupCheckboxGrid
          v-model="selectedGroupIds"
          :groups="groups"
          :loading="groupsLoading"
          :disabled="saving"
        />
      </BaseFormItem>
    </div>

    <template #footer>
      <BaseButton variant="ghost" :disabled="saving" @click="open = false">
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
