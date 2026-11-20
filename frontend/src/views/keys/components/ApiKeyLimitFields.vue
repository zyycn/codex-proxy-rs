<script setup lang="ts">
import type { ApiKeyLimitField } from '../composables/useApiKeyFormRules'
import { ZButton, ZFormItem, ZInput } from '@codex-proxy/ui'
import { DollarSign } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

defineProps<{ disabled: boolean, mixed?: ApiKeyLimitField[] }>()
const emit = defineEmits<{ edit: [field: ApiKeyLimitField] }>()
const dailyLimitUsd = defineModel<string>('dailyLimitUsd', { required: true })
const weeklyLimitUsd = defineModel<string>('weeklyLimitUsd', { required: true })
const maxConcurrency = defineModel<string>('maxConcurrency', { required: true })
const requestsPerMinute = defineModel<string>('requestsPerMinute', { required: true })
const { t } = useI18n({ useScope: 'global' })
const fields = computed(() => [
  {
    key: 'dailyLimitUsd',
    model: dailyLimitUsd,
    label: t('keys.editor.dailyLimit'),
    accessible: t('keys.editor.dailyLimitUsd'),
    money: true,
  },
  {
    key: 'weeklyLimitUsd',
    model: weeklyLimitUsd,
    label: t('keys.editor.weeklyLimit'),
    accessible: t('keys.editor.weeklyLimitUsd'),
    money: true,
  },
  {
    key: 'maxConcurrency',
    model: maxConcurrency,
    label: t('keys.editor.maximumConcurrency'),
    accessible: t('keys.editor.maximumConcurrency'),
    money: false,
  },
  {
    key: 'requestsPerMinute',
    model: requestsPerMinute,
    label: t('keys.editor.requestsPerMinuteRpm'),
    accessible: t('keys.editor.requestsPerMinute'),
    money: false,
  },
] as const)
function update(field: typeof fields.value[number], value: string) {
  field.model.value = value
  emit('edit', field.key)
}
</script>

<template>
  <div class="grid gap-6 sm:grid-cols-2">
    <ZFormItem v-for="field in fields" :key="field.key" :prop="field.key" :label="field.label">
      <template v-if="mixed?.includes(field.key)" #label-extra>
        <span class="text-z-xs text-z-text-secondary">{{ t('keys.batch.mixed') }}</span>
      </template>
      <template v-if="mixed?.includes(field.key)" #extra>
        <ZButton variant="link" size="small" :disabled="disabled" @click="update(field, '')">
          {{ t('keys.batch.clearLimit') }}
        </ZButton>
      </template>
      <ZInput
        :model-value="field.model.value" type="number" min="0" :step="field.money ? 'any' : '1'"
        :aria-label="field.accessible" :placeholder="t('common.values.unlimited')" :disabled="disabled"
        @update:model-value="update(field, $event)"
      >
        <template v-if="field.money" #prefix>
          <DollarSign class="size-4" aria-hidden="true" />
        </template>
      </ZInput>
    </ZFormItem>
  </div>
</template>
