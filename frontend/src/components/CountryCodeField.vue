<script setup lang="ts">
import type { FormRule } from '@codex-proxy/ui'
import { ZFormItem, ZInput } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{ disabled?: boolean, formProp?: string, label?: string }>()

const country = defineModel<string>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const countryRule = computed<FormRule>(() => ({
  required: true,
  pattern: /^[A-Za-z]{2}$/u,
  transform: value => value.trim(),
  trigger: ['blur', 'change'],
  message: t('common.location.countryCodePlaceholder'),
}))
</script>

<template>
  <ZFormItem :prop="formProp" :rules="countryRule" :label="label ?? t('common.location.countryCode')" required>
    <ZInput v-model="country" :disabled="disabled" :aria-label="label ?? t('common.location.countryCode')" :placeholder="t('common.location.countryCodePlaceholder')" maxlength="2" />
  </ZFormItem>
</template>
