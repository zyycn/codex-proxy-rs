<script setup lang="ts">
import type { AccountGroupRef, AccountModelAccess } from '@/api'

import { ZButton, ZDialog, ZFormItem, ZSegmented } from '@codex-proxy/ui'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
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

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const enabled = defineModel<boolean | undefined>('enabled', { required: true })
const concurrencyLimit = defineModel<string>('concurrencyLimit', { required: true })
const modelAccess = defineModel<AccountModelAccess | undefined>('modelAccess', { required: true })
const weight = defineModel<string>('weight', { required: true })
const proxyMode = defineModel<string>('proxyMode', { required: true })
const proxyId = defineModel<string>('proxyId', { required: true })
const selectedGroupIds = defineModel<string[]>('selectedGroupIds', { required: true })
const schedulingModes = computed(() => [
  {
    value: 'preserve',
    label: t('accounts.scheduling.preserve'),
  },
  {
    value: 'enabled',
    label: t('accounts.actions.enableScheduling'),
  },
  {
    value: 'disabled',
    label: t('accounts.actions.disableScheduling'),
  },
])
const schedulingMode = computed({
  get: () => enabled.value === undefined
    ? 'preserve'
    : enabled.value ? 'enabled' : 'disabled',
  set: (value: string) => {
    enabled.value = value === 'preserve'
      ? undefined
      : value === 'enabled'
  },
})
</script>

<template>
  <ZDialog
    v-model="open"
    :title="t('accounts.batchEdit.title')"
    :description="t('accounts.batchEdit.selectionTitle', { count: selectedCount })"
    width="48rem"
    :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving"
  >
    <div class="grid gap-5">
      <AccountSettingsFields
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
      >
        <template #scheduling>
          <ZFormItem :label="t('accounts.scheduling.scheduling')">
            <ZSegmented
              v-model="schedulingMode"
              class="w-80 min-w-0 max-w-full"
              :aria-label="t('accounts.scheduling.scheduling')"
              :options="schedulingModes"
              :disabled="saving"
            />
          </ZFormItem>
        </template>
      </AccountSettingsFields>
    </div>

    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton
        type="primary"
        :loading="saving"
        :disabled="selectedCount === 0 || groupsLoading"
        @click="emit('save')"
      >
        {{ t('common.actions.save') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
