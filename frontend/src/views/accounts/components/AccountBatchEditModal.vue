<script setup lang="ts">
import type { AccountGroup } from '@/api'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import AccountSettingsFields from './AccountSettingsFields.vue'

defineProps<{
  selectedCount: number
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
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
</script>

<template>
  <BaseModal
    v-model="open"
    title="批量编辑账号"
    :description="`统一调整选中的 ${selectedCount} 个账号，保存后将覆盖它们当前的调度参数和分组设置。`"
    size="md"
    :dismissible="!saving"
  >
    <AccountSettingsFields
      v-model:enabled="enabled"
      v-model:concurrency-limit="concurrencyLimit"
      v-model:weight="weight"
      v-model:selected-group-ids="selectedGroupIds"
      v-model:proxy-mode="proxyMode"
      v-model:proxy-id="proxyId"
      :groups="groups"
      :groups-loading="groupsLoading"
      :disabled="saving"
    />

    <template #footer>
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton
        variant="primary"
        :loading="saving"
        :disabled="selectedCount === 0 || groupsLoading"
        @click="emit('save')"
      >
        保存更改
      </BaseButton>
    </template>
  </BaseModal>
</template>
