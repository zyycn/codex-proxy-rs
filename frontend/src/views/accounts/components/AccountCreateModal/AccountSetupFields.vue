<script setup lang="ts">
import type { AccountCreateForm } from './model'
import type { AccountGroup } from '@/api'
import AccountSettingsFields from '../AccountSettingsFields.vue'
import AccountProviderChooser from './AccountProviderChooser.vue'

defineProps<{
  groups: AccountGroup[]
  groupsLoading: boolean
  disabled: boolean
  proxyError?: string
}>()
const form = defineModel<AccountCreateForm>({ required: true })
</script>

<template>
  <div class="grid gap-6">
    <fieldset class="m-0 min-w-0 border-0 p-0">
      <legend class="mb-3 p-0 text-cp font-medium text-cp-text-secondary">
        账号平台
      </legend>
      <AccountProviderChooser :selected="form.provider" :disabled="disabled" @select="form.provider = $event" />
    </fieldset>
    <AccountSettingsFields
      v-model:enabled="form.enabled"
      v-model:concurrency-limit="form.concurrencyLimit"
      v-model:weight="form.weight"
      v-model:selected-group-ids="form.groupIds"
      v-model:proxy-mode="form.proxyMode"
      v-model:proxy-id="form.proxyId"
      :groups="groups"
      :groups-loading="groupsLoading"
      :preserve-proxy="false"
      :disabled="disabled"
      :proxy-error="proxyError"
    />
  </div>
</template>
