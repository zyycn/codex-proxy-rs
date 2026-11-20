<script setup lang="ts">
import type { SelectOption } from '@codex-proxy/ui/select'
import type { OpenAiRequestCountry, RequestLocation } from '@/api'

import { ZCard, ZFormItem, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import CountryCodeField from '@/components/CountryCodeField.vue'
import HelpPopover from '@/components/HelpPopover.vue'
import RequestLocationDetectButton from '@/components/RequestLocationDetectButton.vue'
import RequestLocationFields from '@/components/RequestLocationFields.vue'

defineProps<{ disabled: boolean, detectingLocation: boolean }>()

const emit = defineEmits<{ detectLocation: [] }>()

const { t } = useI18n({ useScope: 'global' })

const location = defineModel<RequestLocation>({ required: true })
const enabled = defineModel<boolean>('enabled', { required: true })
const requestCountry = defineModel<OpenAiRequestCountry>('requestCountry', { required: true })
const modes = computed<Array<SelectOption<OpenAiRequestCountry['mode']> & { help: string }>>(() => [
  {
    value: 'default',
    label: t('settings.location.countryModes.default'),
    help: t('settings.location.countryHints.default'),
  },
  {
    value: 'passthrough',
    label: t('settings.location.countryModes.passthrough'),
    help: t('settings.location.countryHints.passthrough'),
  },
  {
    value: 'follow_location',
    label: t('settings.location.countryModes.follow_location'),
    help: t('settings.location.countryHints.follow_location'),
  },
  {
    value: 'custom',
    label: t('settings.location.countryModes.custom'),
    help: t('settings.location.countryHints.custom'),
  },
])

function setMode(mode: OpenAiRequestCountry['mode'] | undefined) {
  if (mode && mode !== requestCountry.value.mode) {
    requestCountry.value = mode === 'custom'
      ? { mode, country: '' }
      : { mode }
  }
}
</script>

<template>
  <ZCard :title="t('settings.location.requestLocationOverride')" :description="t('settings.location.description')">
    <template #body>
      <div class="flex max-w-6xl items-center justify-between gap-3">
        <ZSwitch v-model="enabled" :label="t('settings.location.enableLocationOverlay')" :disabled="disabled" />
        <RequestLocationDetectButton :loading="detectingLocation" :disabled="disabled" @detect="emit('detectLocation')" />
      </div>
      <div class="grid min-w-0 gap-4.5 mt-4 max-w-6xl">
        <RequestLocationFields v-model="location" :form-prop="enabled ? 'requestLocation' : undefined" :disabled="disabled || !enabled" />
        <div class="grid gap-4.5 sm:grid-cols-2">
          <ZFormItem :label="t('settings.location.requestCountry')">
            <template #label-extra>
              <HelpPopover :label="t('settings.location.countryHelp')">
                <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-2">
                  <template v-for="mode in modes" :key="mode.value">
                    <dt class="font-emphasis text-z-text">
                      {{ mode.label }}
                    </dt>
                    <dd>{{ mode.help }}</dd>
                  </template>
                </dl>
              </HelpPopover>
            </template>
            <ZSelect :model-value="requestCountry.mode" :options="modes" :aria-label="t('settings.location.requestCountry')" :disabled="disabled" @update:model-value="setMode" />
          </ZFormItem>
          <CountryCodeField
            v-if="requestCountry.mode === 'custom'"
            :model-value="requestCountry.country"
            form-prop="openaiRequestCountry.country"
            :label="t('settings.location.customCountryCode')"
            :disabled="disabled"
            @update:model-value="requestCountry = { mode: 'custom', country: $event }"
          />
        </div>
      </div>
    </template>
  </ZCard>
</template>
