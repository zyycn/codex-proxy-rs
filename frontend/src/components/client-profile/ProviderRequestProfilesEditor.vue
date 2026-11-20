<script setup lang="ts">
import type { ClientProfileSelection, ProviderRequestProfile, ProviderRequestProfiles, ProviderRequestProfileUpdates, XaiClientProfileSelection } from '@/api/modules/settings/profiles'

import { ZSegmented } from '@codex-proxy/ui'
import { isEqual } from 'es-toolkit'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/utils/providers'
import ClientProfileEditor from './ClientProfileEditor.vue'
import XaiClientProfileEditor from './XaiClientProfileEditor.vue'

const props = withDefaults(defineProps<{
  active?: boolean
  disabled?: boolean
  initialProfiles?: ProviderRequestProfileUpdates
  allowInherit?: boolean
}>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ProviderRequestProfiles>({
  default: () => ({}),
})
const updates = defineModel<ProviderRequestProfileUpdates>('updates')
const provider = shallowRef<(typeof PROVIDER_IDS)[number]>('openai')
const providerOptions = PROVIDER_IDS.map(value => ({
  value,
  label: formatProviderLabel(value),
  icon: providerIcon(value),
}))
const providerSelectorStyle = { width: `${providerOptions.length * 40}px` }
const openai = profileModel<ClientProfileSelection>('openai')
const xai = profileModel<XaiClientProfileSelection>('xai')

function profileModel<T extends object>(providerId: string) {
  return computed<T | null | undefined>({
    get: () => {
      if (updates.value === undefined)
        return (model.value[providerId] as T | undefined) ?? null
      const source = Object.hasOwn(updates.value, providerId)
        ? updates.value
        : props.initialProfiles
      return source?.[providerId] as T | null | undefined
    },
    set: (value) => {
      if (updates.value !== undefined) {
        const next = { ...updates.value }
        if (value === undefined || isEqual(value, props.initialProfiles?.[providerId]))
          delete next[providerId]
        else
          next[providerId] = value as ProviderRequestProfile | null
        updates.value = next
      }
      else {
        const next = { ...model.value }
        if (value == null)
          delete next[providerId]
        else
          next[providerId] = value as ProviderRequestProfile
        model.value = next
      }
    },
  })
}
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <div v-if="!allowInherit" class="flex flex-wrap items-start justify-between gap-3">
      <slot name="heading" />
      <ZSegmented
        v-model="provider"
        class="ml-auto max-w-full shrink-0"
        :aria-label="t('settings.clientProfile.clientIdentityProvider')"
        :options="providerOptions"
        :disabled="disabled"
        :style="providerSelectorStyle"
        display="icon"
      />
    </div>
    <ClientProfileEditor
      v-if="provider === 'openai'"
      v-model="openai"
      :allow-inherit="allowInherit"
      :allow-preserve="updates !== undefined && initialProfiles?.[provider] === undefined"
      :active="active"
      :disabled="disabled"
    >
      <template #source-extra>
        <ZSegmented
          v-model="provider"
          class="max-w-full shrink-0"
          :aria-label="t('settings.clientProfile.upstreamIdentityPlatform')"
          :options="providerOptions"
          :disabled="disabled"
          :style="providerSelectorStyle"
          display="icon"
        />
      </template>
    </ClientProfileEditor>
    <XaiClientProfileEditor
      v-else
      v-model="xai"
      :allow-inherit="allowInherit"
      :allow-preserve="updates !== undefined && initialProfiles?.[provider] === undefined"
      :active="active"
      :disabled="disabled"
    >
      <template #source-extra>
        <ZSegmented
          v-model="provider"
          class="max-w-full shrink-0"
          :aria-label="t('settings.clientProfile.upstreamIdentityPlatform')"
          :options="providerOptions"
          :disabled="disabled"
          :style="providerSelectorStyle"
          display="icon"
        />
      </template>
    </XaiClientProfileEditor>
  </div>
</template>
