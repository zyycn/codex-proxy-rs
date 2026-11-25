<script setup lang="ts">
import type { ProviderRequestProfiles } from '@/api'
import type { BuiltinProvider } from '@/providers/catalog'

import { ZCard } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import ProviderRequestProfilesEditor from '@/components/request-profile/index.vue'

withDefaults(defineProps<{ provider: BuiltinProvider, active?: boolean, disabled?: boolean }>(), {
  active: true,
  disabled: false,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ProviderRequestProfiles>({ required: true })
</script>

<template>
  <ZCard :title="t('settings.clientProfile.clientIdentity')" :description="t('settings.clientProfile.identityDescription')">
    <ProviderRequestProfilesEditor
      v-model="model"
      :provider="provider"
      :active="active"
      :disabled="disabled"
    />
  </ZCard>
</template>
