<script setup lang="ts">
import type { AccountGroupRef, AccountModelAccess } from '@/api'

import { ZButton, ZDialog } from '@codex-proxy/ui'
import AccountSettingsFields from './AccountSettingsFields.vue'

defineProps<{
  selectedCount: number
  catalogAccountId?: string
  groups: AccountGroupRef[]
  groupsLoading: boolean
  saving: boolean
}>()

const emit = defineEmits<{
  save: []
}>()

const open = defineModel<boolean>({ required: true })
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
    title="批量编辑账号"
    :description="`编辑 ${selectedCount} 个账号`"
    width="48rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
  >
    <div class="grid gap-5">
      <AccountSettingsFields
        v-model:enabled="enabled"
        v-model:concurrency-limit="concurrencyLimit"
        v-model:weight="weight"
        v-model:model-access="modelAccess"
        v-model:selected-group-ids="selectedGroupIds"
        v-model:proxy-mode="proxyMode"
        v-model:proxy-id="proxyId"
        preserve-model-access
        :account-id="catalogAccountId"
        :groups="groups"
        :groups-loading="groupsLoading"
        :disabled="saving"
      />
    </div>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        取消
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        :disabled="selectedCount === 0 || groupsLoading"
        @click="emit('save')"
      >
        保存更改
      </ZButton>
    </template>
  </ZDialog>
</template>
