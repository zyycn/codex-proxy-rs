<script setup lang="ts">
import type { Component } from 'vue'
import type { ProviderRequestProfile, ProviderRequestProfiles, ProviderRequestProfileUpdates } from '@/api'
import type { BuiltinProvider } from '@/providers/catalog'
import { ZSegmented } from '@codex-proxy/ui'
import { isEqual } from 'es-toolkit'
import { computed, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import AntigravityRequestProfileEditor from '@/providers/antigravity/request-profile/AntigravityRequestProfileEditor.vue'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/providers/catalog'
import OpenAiRequestProfileEditor from '@/providers/openai/request-profile/OpenAiRequestProfileEditor.vue'
import XaiRequestProfileEditor from '@/providers/xai/request-profile/XaiRequestProfileEditor.vue'

const props = withDefaults(defineProps<{
  active?: boolean
  disabled?: boolean
  initialProfiles?: ProviderRequestProfileUpdates
  allowInherit?: boolean
  provider?: BuiltinProvider
}>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})

const editors: Record<BuiltinProvider, Component> = {
  openai: OpenAiRequestProfileEditor,
  xai: XaiRequestProfileEditor,
  antigravity: AntigravityRequestProfileEditor,
}

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ProviderRequestProfiles>({
  default: () => ({}),
})
const updates = defineModel<ProviderRequestProfileUpdates>('updates')
const selectedProvider = shallowRef<BuiltinProvider>(PROVIDER_IDS[0]!)
const provider = computed(() => props.provider ?? selectedProvider.value)
const providerOptions = PROVIDER_IDS.map(value => ({
  value,
  label: formatProviderLabel(value),
  icon: providerIcon(value),
}))
const providerSelectorStyle = { width: `${providerOptions.length * 40}px` }
const drafts = shallowRef<ProviderRequestProfiles>({})
const profile = computed<ProviderRequestProfile | null | undefined>({
  get: () => {
    const id = provider.value
    if (updates.value === undefined)
      return model.value[id] ?? null
    return Object.hasOwn(updates.value, id)
      ? updates.value[id]
      : props.initialProfiles?.[id]
  },
  set: (value) => {
    const id = provider.value
    const previous = profile.value
    if (value == null && previous)
      drafts.value = { ...drafts.value, [id]: previous }
    if (updates.value !== undefined) {
      const next = { ...updates.value }
      if (value === undefined || isEqual(value, props.initialProfiles?.[id]))
        delete next[id]
      else
        next[id] = value
      updates.value = next
    }
    else {
      const next = { ...model.value }
      if (value == null)
        delete next[id]
      else
        next[id] = value
      model.value = next
    }
  },
})
watch(() => props.active, (active) => {
  if (!active)
    drafts.value = {}
})
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <ZSegmented
      v-if="!allowInherit && !props.provider"
      v-model="selectedProvider"
      class="max-w-full justify-self-end"
      :aria-label="t('settings.clientProfile.clientIdentityProvider')"
      :options="providerOptions"
      :disabled="disabled"
      :style="providerSelectorStyle"
      display="icon"
    />
    <component
      :is="editors[provider]"
      v-model="profile"
      :draft="drafts[provider]"
      :allow-inherit="allowInherit"
      :allow-preserve="updates !== undefined && initialProfiles?.[provider] === undefined"
      :active="active"
      :disabled="disabled"
    >
      <template #source-extra>
        <ZSegmented
          v-if="!props.provider"
          v-model="selectedProvider"
          class="max-w-full shrink-0"
          :aria-label="t('settings.clientProfile.upstreamIdentityPlatform')"
          :options="providerOptions"
          :disabled="disabled"
          :style="providerSelectorStyle"
          display="icon"
        />
      </template>
    </component>
  </div>
</template>
