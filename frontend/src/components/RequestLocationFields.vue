<script setup lang="ts">
import type { FormRule } from '@codex-proxy/ui'
import type { RequestLocation } from '@/api'

import { ZFormItem, ZInput } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import CountryCodeField from '@/components/CountryCodeField.vue'

defineProps<{ disabled?: boolean, formProp?: string }>()

const { t } = useI18n({ useScope: 'global' })

const location = defineModel<RequestLocation>({ required: true })
const requiredRule = computed<FormRule>(() => ({
  required: true,
  whitespace: true,
  trigger: ['blur', 'change'],
  message: t('common.location.required'),
}))

function update(field: keyof RequestLocation, value: string) {
  location.value = { ...location.value, [field]: value }
}
</script>

<template>
  <div class="grid gap-4.5 sm:grid-cols-2">
    <CountryCodeField
      :model-value="location.country"
      :form-prop="formProp ? `${formProp}.country` : undefined"
      :show-message="Boolean(location.country.trim())"
      :disabled="disabled"
      @update:model-value="update('country', $event)"
    />
    <ZFormItem :prop="formProp ? `${formProp}.region` : undefined" :rules="requiredRule" :show-message="false" :label="t('common.location.region')" required>
      <ZInput :model-value="location.region" :disabled="disabled" :aria-label="t('common.location.region')" :placeholder="t('common.location.regionPlaceholder')" maxlength="128" @update:model-value="update('region', $event)" />
    </ZFormItem>
    <ZFormItem :prop="formProp ? `${formProp}.city` : undefined" :rules="requiredRule" :show-message="false" :label="t('common.location.city')" required>
      <ZInput :model-value="location.city" :disabled="disabled" :aria-label="t('common.location.city')" :placeholder="t('common.location.cityPlaceholder')" maxlength="128" @update:model-value="update('city', $event)" />
    </ZFormItem>
    <ZFormItem :prop="formProp ? `${formProp}.timezone` : undefined" :rules="requiredRule" :show-message="false" :label="t('common.location.timeZone')" required>
      <ZInput :model-value="location.timezone" :disabled="disabled" :aria-label="t('common.location.timeZone')" :placeholder="t('common.location.timeZonePlaceholder')" @update:model-value="update('timezone', $event)" />
    </ZFormItem>
  </div>
</template>
