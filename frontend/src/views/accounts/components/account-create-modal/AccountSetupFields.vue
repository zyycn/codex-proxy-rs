<script setup lang="ts">
import type { AccountCreateForm } from '../../utils/accountCreate'

import type { AccountGroupRef } from '@/api'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { accountCreateProvider } from '../../utils/accountCreate'
import AccountSettingsFields from '../AccountSettingsFields.vue'
import AccountProviderChooser from './AccountProviderChooser.vue'

defineProps<{
  groups: AccountGroupRef[]
  groupsLoading: boolean
  disabled: boolean
}>()

const { t } = useI18n({ useScope: 'global' })

const form = defineModel<AccountCreateForm>({ required: true })
const name = computed({
  get: () => form.value.source?.kind === 'provider'
    ? form.value.name
    : undefined,
  set: (value: string | undefined) => {
    if (value !== undefined)
      form.value.name = value
  },
})
</script>

<template>
  <div class="grid gap-6">
    <fieldset class="m-0 min-w-0 border-0 p-0">
      <legend class="mb-3 p-0 text-z font-medium text-z-text-secondary">
        {{ t('accounts.onboarding.accountPlatform') }}
      </legend>
      <AccountProviderChooser v-model="form.source" :disabled="disabled" />
    </fieldset>
    <AccountSettingsFields
      v-model:name="name"
      v-model:notes="form.notes"
      v-model:enabled="form.enabled"
      v-model:concurrency-limit="form.concurrencyLimit"
      v-model:weight="form.weight"
      v-model:model-access="form.modelAccess"
      v-model:selected-group-ids="form.groupIds"
      v-model:proxy-mode="form.proxyMode"
      v-model:proxy-id="form.proxyId"
      preserve-model-access

      :groups="groups"
      :provider="accountCreateProvider(form)"
      :groups-loading="groupsLoading"
      :preserve-proxy="false"
      :disabled="disabled"
      :name-placeholder="t('accounts.onboarding.namePlaceholder')"
      :notes-placeholder="t('accounts.onboarding.maximumWordsOptional')"
    />
  </div>
</template>
