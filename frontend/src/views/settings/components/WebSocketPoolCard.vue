<script setup lang="ts">
import type { OpenAiWebSocketPoolSettings } from '@/api'
import { ZCard, ZFormItem, ZInput } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import HelpPopover from '@/components/HelpPopover.vue'

defineProps<{ disabled: boolean }>()
const settings = defineModel<Record<keyof OpenAiWebSocketPoolSettings, number | null>>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const fields = computed(() => [
  {
    key: 'maxAgeMs',
    label: t('settings.websocketPool.maxAgeMs'),
    help: t('settings.websocketPool.maxAgeMsHelp'),
    min: 1,
    max: Number.MAX_SAFE_INTEGER,
  },
  {
    key: 'maxConnecting',
    label: t('settings.websocketPool.maxConnecting'),
    help: t('settings.websocketPool.maxConnectingHelp'),
    min: 1,
    max: 4_294_967_295,
  },
  {
    key: 'fastPathBudgetMs',
    label: t('settings.websocketPool.fastPathBudgetMs'),
    help: t('settings.websocketPool.fastPathBudgetMsHelp'),
    min: 1,
    max: 15_000,
  },
  {
    key: 'idleTimeoutMs',
    label: t('settings.websocketPool.idleTimeoutMs'),
    help: t('settings.websocketPool.idleTimeoutMsHelp'),
    min: 1,
    max: Number.MAX_SAFE_INTEGER,
  },
  {
    key: 'streamIdleTimeoutMs',
    label: t('settings.websocketPool.streamIdleTimeoutMs'),
    help: t('settings.websocketPool.streamIdleTimeoutMsHelp'),
    min: 0,
    max: Number.MAX_SAFE_INTEGER,
  },
] as const)

function update(key: keyof OpenAiWebSocketPoolSettings, value: string) {
  const number = value.trim()
    ? Number(value)
    : Number.NaN
  settings.value = {
    ...settings.value,
    [key]: Number.isFinite(number)
      ? number
      : null,
  }
}
</script>

<template>
  <ZCard class="@container" :title="t('settings.websocketPool.title')" :description="t('settings.websocketPool.description')">
    <div class="grid min-w-0 gap-4.5 @md:grid-cols-2 @4xl:grid-cols-5">
      <ZFormItem v-for="field in fields" :key="field.key" :prop="`openaiWebsocketPool.${field.key}`" :label="field.label">
        <template #label-extra>
          <HelpPopover :label="t('common.form.fieldHelp', { label: field.label })">
            <p class="whitespace-pre-line">
              {{ field.help }}
            </p>
          </HelpPopover>
        </template>
        <ZInput :model-value="String(settings[field.key] ?? '')" :aria-label="field.key === 'maxConnecting' ? field.label : t('settings.units.withUnit', { label: field.label, unit: t('settings.units.milliseconds') })" type="number" :min="field.min" :max="field.max" step="1" :disabled="disabled" @update:model-value="update(field.key, $event)">
          <template v-if="field.key !== 'maxConnecting'" #suffix>
            {{ t('settings.units.milliseconds') }}
          </template>
        </ZInput>
      </ZFormItem>
    </div>
  </ZCard>
</template>
