<script setup lang="ts">
import { useTimeAgoIntl } from '@vueuse/core'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ value: string | null, fullDisplay: string | null }>()
const { locale } = useI18n({ useScope: 'global' })
const shortTimeUnits: Record<string, string> = {
  sec: 's',
  min: 'min',
  hr: 'h',
  day: 'd',
}

const display = useTimeAgoIntl(() => props.value ?? 0, {
  get locale() { return locale.value },
  relativeTimeFormatOptions: { numeric: 'auto', style: 'short' },
  insertSpace: false,
  joinParts(parts) {
    return parts.map((part) => {
      if (locale.value !== 'en' || part.type !== 'literal')
        return part.value
      return part.value.replace(/\s+(sec|min|hr|day)s?\b\.?/g, (_, unit: string) => ` ${shortTimeUnits[unit] ?? unit}`)
    }).join('')
  },
})
</script>

<template>
  <span v-if="value" class="whitespace-nowrap" :title="fullDisplay ?? undefined">{{ display }}</span>
</template>
