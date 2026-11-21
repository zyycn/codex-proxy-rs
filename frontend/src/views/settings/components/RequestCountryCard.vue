<script setup lang="ts">
import type { SelectOption } from '@codex-proxy/ui/select'
import type { OpenAiRequestCountry } from '@/api'
import { ZCard, ZFormItem, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import CountryCodeField from '@/components/CountryCodeField.vue'
import HelpPopover from '@/components/HelpPopover.vue'

defineProps<{ disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })
const requestCountry = defineModel<OpenAiRequestCountry>({ required: true })
const modes = computed<Array<SelectOption<OpenAiRequestCountry['mode']> & { help: string }>>(() => [
  {
    value: 'follow_location',
    label: t('settings.location.countryModes.follow_location'),
    help: t('settings.location.countryHints.follow_location'),
  },
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
  <ZCard :description="t('settings.location.requestCountryDescription')">
    <template #title>
      <span class="inline-flex items-center gap-1.5">
        {{ t('settings.location.requestCountry') }}
        <HelpPopover :label="t('settings.location.countryHelp')">
          <p class="mb-2 font-emphasis text-z-text">
            {{ t('settings.location.requestCountryScope') }}
          </p>
          <dl class="grid grid-cols-[auto_1fr] gap-x-3 gap-y-2">
            <template v-for="mode in modes" :key="mode.value">
              <dt class="font-emphasis text-z-text">
                {{ mode.label }}
              </dt>
              <dd>{{ mode.help }}</dd>
            </template>
          </dl>
        </HelpPopover>
      </span>
    </template>
    <div class="grid min-w-0 max-w-6xl items-start gap-4.5 sm:grid-cols-2">
      <ZFormItem :label="t('settings.location.countryMode')">
        <ZSelect :model-value="requestCountry.mode" :options="modes" :aria-label="t('settings.location.countryMode')" :disabled="disabled" @update:model-value="setMode" />
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
  </ZCard>
</template>
