<script setup lang="ts">
import { ZFormItem, ZIconButton, ZInput } from '@codex-proxy/ui'
import { Dices } from '@lucide/vue'
import { useI18n } from 'vue-i18n'
import HelpPopover from '@/components/HelpPopover.vue'
import { generateRequestId as generateUuid } from '@/utils/operation'

defineProps<{ disabled: boolean, placeholder: string, error: string }>()
const seed = defineModel<string>({ required: true })
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <ZFormItem :label="t('settings.clientProfile.installationSeed')" :error="error">
    <template #label-extra>
      <HelpPopover :label="t('settings.clientProfile.installationSeed')" :content="t('settings.clientProfile.installationSeedHelp')" />
    </template>
    <template #extra>
      <ZIconButton
        variant="link" size="small" class="-my-2"
        :aria-label="t('settings.clientProfile.generateInstallationSeed')"
        :disabled="disabled"
        @click="seed = generateUuid()"
      >
        <Dices class="size-4" aria-hidden="true" />
      </ZIconButton>
    </template>
    <ZInput
      v-model="seed"
      :aria-label="t('settings.clientProfile.installationSeed')"
      :aria-invalid="!!error"
      :placeholder="placeholder"
      :disabled="disabled"
      :spellcheck="false"
      autocomplete="off"
      class="font-mono"
    />
  </ZFormItem>
</template>
