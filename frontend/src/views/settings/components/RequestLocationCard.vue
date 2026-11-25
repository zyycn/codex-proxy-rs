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
  <ZCard :title="t('settings.location.requestLocationOverride')" :description="t('settings.location.description')" class="@container">
    <template #actions>
      <div class="flex flex-wrap items-center gap-3">
        <RequestLocationDetectButton :loading="detectingLocation" :disabled="disabled" @detect="emit('detectLocation')" />
        <ZSwitch
          v-model="enabled"
          :aria-label="t('settings.location.enableLocationOverlay')"
          :active-text="t('common.values.on')"
          :inactive-text="t('common.values.off')"
          inline-prompt
          :width="56"
          :disabled="disabled"
        />
      </div>
    </template>
    <RequestLocationFields v-model="location" class="@lg:grid-cols-[repeat(3,minmax(0,1fr))_minmax(0,1.5fr)]" :form-prop="enabled ? 'requestLocation' : undefined" :disabled="disabled || !enabled" />
  </ZCard>
</template>
