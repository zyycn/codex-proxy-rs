<script setup lang="ts">
import type { RequestLocation } from '@/api'

import { ZCard, ZSwitch } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'
import RequestLocationDetectButton from '@/components/RequestLocationDetectButton.vue'
import RequestLocationFields from '@/components/RequestLocationFields.vue'

defineProps<{ disabled: boolean, detectingLocation: boolean }>()

const emit = defineEmits<{ detectLocation: [] }>()

const { t } = useI18n({ useScope: 'global' })

const location = defineModel<RequestLocation>({ required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
</script>

<template>
  <ZCard :title="t('settings.location.requestLocationOverride')" :description="t('settings.location.description')">
    <template #body>
      <div class="flex max-w-6xl items-center justify-between gap-3">
        <ZSwitch v-model="enabled" :label="t('settings.location.enableLocationOverlay')" :disabled="disabled" />
        <RequestLocationDetectButton :loading="detectingLocation" :disabled="disabled" @detect="emit('detectLocation')" />
      </div>
      <RequestLocationFields v-model="location" class="mt-4 max-w-6xl" :form-prop="enabled ? 'requestLocation' : undefined" :disabled="disabled || !enabled" />
    </template>
  </ZCard>
</template>
