<script setup lang="ts">
import { ZCard, ZFormItem, ZIconButton, ZInput } from '@codex-proxy/ui'

import { CircleHelp, MonitorUp, TerminalSquare } from '@lucide/vue'

import { useI18n } from 'vue-i18n'

defineOptions({ name: 'ClientVersionPolicyCard' })

withDefaults(
  defineProps<{
    loading?: boolean
  }>(),
  {
    loading: false,
  },
)

const emit = defineEmits<{
  help: []
}>()

const { t } = useI18n({ useScope: 'global' })

const minCodexDesktopVersion = defineModel<string>('minCodexDesktopVersion', { required: true })
const minCodexCliVersion = defineModel<string>('minCodexCliVersion', { required: true })
</script>

<template>
  <ZCard>
    <template #title>
      <span class="inline-flex items-center gap-1.5">
        <span>{{ t('settings.clientVersion.clientVersionRestrictions') }}</span>
        <ZIconButton
          variant="link"
          :aria-label="t('settings.clientVersion.openGuide')"
          class="size-6!"
          @click="emit('help')"
        >
          <CircleHelp class="size-3.5" aria-hidden="true" />
        </ZIconButton>
      </span>
    </template>

    <div class="grid min-w-0 gap-4.5 max-w-6xl sm:grid-cols-2">
      <ZFormItem
        prop="minCodexDesktopVersion"
        :label="t('settings.clientVersion.codexDesktopMinimumVersion')"
        :description="t('settings.clientVersion.onlyCheckDesktopVersionEG')"
      >
        <ZInput
          v-model="minCodexDesktopVersion"
          :aria-label="t('settings.clientVersion.codexDesktopMinimumVersion')"
          autocomplete="off"
          spellcheck="false"
          :placeholder="t('common.values.unlimited')"
          :disabled="loading"
        >
          <template #prefix>
            <MonitorUp class="size-4" />
          </template>
        </ZInput>
      </ZFormItem>

      <ZFormItem
        prop="minCodexCliVersion"
        :label="t('settings.clientVersion.codexCliMinimumVersion')"
        :description="t('settings.clientVersion.minimumVersionHelp')"
      >
        <ZInput
          v-model="minCodexCliVersion"
          :aria-label="t('settings.clientVersion.codexCliMinimumVersion')"
          autocomplete="off"
          spellcheck="false"
          :placeholder="t('common.values.unlimited')"
          :disabled="loading"
        >
          <template #prefix>
            <TerminalSquare class="size-4" />
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
