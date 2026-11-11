<script setup lang="ts">
import { ZCard, ZFormItem, ZSwitch } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'

defineProps<{ disabled: boolean }>()
const enabled = defineModel<boolean>({ required: true })
const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <ZCard :title="t('settings.proxyFallback.title')">
    <ZFormItem :description="t('settings.proxyFallback.help')">
      <ZSwitch v-model="enabled" :disabled="disabled" :label="t('settings.proxyFallback.enabled')" />
    </ZFormItem>
  </ZCard>
</template>
