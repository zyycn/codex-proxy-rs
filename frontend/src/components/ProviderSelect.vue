<script setup lang="ts" generic="Value extends string">
import type { SegmentedOption } from '@codex-proxy/ui'
import { ZSegmented } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { formatProviderLabel, PROVIDER_IDS, providerIcon } from '@/providers/catalog'

const props = withDefaults(defineProps<{
  providers?: readonly Value[]
  leadingOption?: SegmentedOption
  display?: 'icon' | 'label'
  size?: 'small' | 'default' | 'large'
  disabled?: boolean
  ariaLabel?: string
}>(), {
  display: 'icon',
  disabled: false,
})
const provider = defineModel<Value>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const options = computed(() => [
  ...(props.leadingOption
    ? [props.leadingOption]
    : []),
  ...(props.providers ?? PROVIDER_IDS).map(value => ({
    value,
    label: formatProviderLabel(value),
    icon: providerIcon(value),
  })),
])
const style = computed(() => props.display === 'icon'
  ? { width: `${options.value.length * 40 + 4}px` }
  : undefined)

function selectProvider(value: string) {
  if (options.value.some(option => option.value === value))
    provider.value = value as Value
}
</script>

<template>
  <ZSegmented
    :model-value="provider"
    :options="options"
    :display="display"
    :size="size"
    :disabled="disabled"
    :aria-label="ariaLabel ?? t('common.providers.select')"
    :style="style"
    @update:model-value="selectProvider"
  />
</template>
