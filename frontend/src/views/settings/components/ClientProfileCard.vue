<script setup lang="ts">
import type { ProviderRequestProfiles } from '@/api/modules/settings/profiles'

import { ZCard } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import ProviderRequestProfilesEditor from '@/components/client-profile/ProviderRequestProfilesEditor.vue'

withDefaults(defineProps<{ active?: boolean, disabled?: boolean }>(), {
  active: true,
  disabled: false,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ProviderRequestProfiles>({ required: true })
</script>

<template>
  <ZCard>
    <ProviderRequestProfilesEditor
      v-model="model"
      :active="active"
      :disabled="disabled"
      class="max-w-6xl"
    >
      <template #heading>
        <div class="min-w-0 pt-0.5">
          <h2 class="m-0 text-xl leading-[1.15] font-heavy text-z-text text-balance">
            {{ t('settings.clientProfile.clientIdentity') }}
          </h2>
          <p class="mt-1.75 mb-0 text-z leading-[1.3] font-emphasis text-z-text-secondary text-pretty">
            {{ t('settings.clientProfile.description') }}
          </p>
        </div>
      </template>
    </ProviderRequestProfilesEditor>
  </ZCard>
</template>
