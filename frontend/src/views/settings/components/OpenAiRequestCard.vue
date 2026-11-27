<script setup lang="ts">
import type { SelectOption } from '@codex-proxy/ui/select'
import type { OpenAiRequestCountry } from '@/api'
import { ZCard, ZFormItem, ZInput, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import CountryCodeField from '@/components/CountryCodeField.vue'
import HelpPopover from '@/components/HelpPopover.vue'

defineProps<{ disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })
const requestCountry = defineModel<OpenAiRequestCountry>('requestCountry', { required: true })
const residency = defineModel<string>('residency', { required: true })
const accountRoutingOverride = defineModel<string>('accountRoutingOverride', { required: true })
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
  <ZCard class="@container" :title="t('settings.requestOptions.title')" :description="t('settings.requestOptions.description')">
    <div class="grid min-w-0 items-start gap-4.5 @md:grid-cols-3">
      <div class="grid min-w-0 gap-4.5">
        <ZFormItem :label="t('settings.location.requestCountry')">
          <template #label-extra>
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
          </template>
          <ZSelect :model-value="requestCountry.mode" :options="modes" :aria-label="t('settings.location.requestCountry')" :disabled="disabled" @update:model-value="setMode" />
        </ZFormItem>
        <CountryCodeField
          v-if="requestCountry.mode === 'custom'"
          :model-value="requestCountry.country"
          form-prop="openaiRequestSettings.requestCountry.country"
          :label="t('settings.location.customCountryCode')"
          :disabled="disabled"
          @update:model-value="requestCountry = { mode: 'custom', country: $event }"
        />
      </div>
      <ZFormItem :label="t('settings.requestOptions.residency')">
        <template #label-extra>
          <HelpPopover :label="t('common.form.fieldHelp', { label: t('settings.requestOptions.residency') })" :content="t('settings.requestOptions.residencyHelp')" />
        </template>
        <ZInput v-model="residency" :aria-label="t('settings.requestOptions.residency')" :placeholder="t('settings.requestOptions.residencyPlaceholder')" :disabled="disabled" clearable />
      </ZFormItem>
      <ZFormItem :label="t('settings.requestOptions.accountRoutingOverride')">
        <template #label-extra>
          <HelpPopover :label="t('common.form.fieldHelp', { label: t('settings.requestOptions.accountRoutingOverride') })" :content="t('settings.requestOptions.accountRoutingOverrideHelp')" />
        </template>
        <ZInput v-model="accountRoutingOverride" :aria-label="t('settings.requestOptions.accountRoutingOverride')" :placeholder="t('settings.requestOptions.accountRoutingPlaceholder')" :disabled="disabled" clearable />
      </ZFormItem>
    </div>
  </ZCard>
</template>
