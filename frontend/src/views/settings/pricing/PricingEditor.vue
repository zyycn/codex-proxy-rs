<script setup lang="ts">
import type { PricingRow } from './model'

import type { ModelPricing, PriceBand, TokenPrices } from '@/api'
import { ZButton, ZDialog, ZFormItem, ZInput, ZPopover, ZSegmented, ZSelect } from '@codex-proxy/ui'
import { Info } from '@lucide/vue'
import { computed, ref, shallowRef, useId, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderIconGroup from '@/components/ProviderIconGroup.vue'
import { bands, parseMultiplier, validPrice } from './model'
import PricingRateFields from './PricingRateFields.vue'

const props = defineProps<{ row?: PricingRow, provider: string, saving: boolean }>()

const emit = defineEmits<{ save: [model: string, pricing: ModelPricing] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const model = ref('')
const multiplier = ref('1')
const draft = ref<Partial<Record<PriceBand, TokenPrices>>>({})
const selectedBand = ref<PriceBand>('standard')
const validation = ref('')
const helpOpen = shallowRef(false)
const helpId = useId()
const base = computed(() => props.row?.base.bands[selectedBand.value])
const current = computed(() => draft.value[selectedBand.value])
const bps = computed(() => parseMultiplier(multiplier.value))
const sourceOptions = computed(() => [
  {
    label: t('settings.pricing.sourcePrice'),
    value: 'inherit',
    disabled: selectedBand.value === 'standard' && !props.row?.base.bands.standard,
  },
  {
    label: t('settings.rotation.customized'),
    value: 'custom',
  },
])
const priceMode = computed({
  get: () => current.value ? 'custom' : 'inherit',
  set: value => toggleBand(value === 'custom'),
})
const bandOptions = computed(() => bands.value
  .filter(band => props.row?.base.bands.image
    ? ['standard', 'image'].includes(band.value)
    : props.provider !== 'xai' || ![
      'flex',
      'long_flex',
      'ultrafast',
      'long_ultrafast',
      'image',
    ].includes(band.value))
  .map(band => ({
    ...band,
    label: band.value === 'standard' && props.row?.base.bands.image
      ? t('settings.pricing.textToken')
      : band.label,
  })))

watch(open, (value) => {
  if (!value)
    return
  model.value = props.row?.model ?? ''
  multiplier.value = String((props.row?.custom?.multiplierBps ?? 10_000) / 10_000)
  draft.value = JSON.parse(JSON.stringify(props.row?.custom?.bands ?? {}))
  selectedBand.value = props.row?.effective.bands.image ? 'image' : 'standard'
  if (!props.row)
    toggleBand(true)
  validation.value = ''
  helpOpen.value = false
})
watch([model, multiplier, draft], () => {
  validation.value = ''
}, { deep: true })
function toggleBand(enabled: boolean) {
  if (enabled) {
    draft.value[selectedBand.value] = {
      ...(base.value ?? {
        input: '',
        output: '',
        cacheRead: '',
        cacheWrite: '',
      }),
    }
  }
  else {
    delete draft.value[selectedBand.value]
  }
}
function updatePrice(field: keyof TokenPrices, value: string) {
  if (current.value && !props.saving)
    current.value[field] = value
}
function submit() {
  if (!model.value || new TextEncoder().encode(model.value).length > 128 || /\s/.test(model.value) || [...model.value].some(char => char.charCodeAt(0) < 32 || char.charCodeAt(0) === 127)) {
    validation.value = t('settings.pricing.validation.modelId')
    return
  }
  if (bps.value === undefined) {
    validation.value = t('settings.pricing.multiplierRangeIsWithUpToFour')
    return
  }
  for (const [band, rates] of Object.entries(draft.value)) {
    if (Object.values(rates).some(price => !validPrice(price))) {
      validation.value = t('settings.pricing.validation.tierPrices')
      selectedBand.value = band as PriceBand
      return
    }
  }
  if (!props.row?.base.bands.standard && !draft.value.standard) {
    validation.value = t('settings.pricing.validation.standardTier')
    selectedBand.value = 'standard'
    return
  }
  validation.value = ''
  emit('save', model.value, { multiplierBps: bps.value, bands: draft.value })
}
</script>

<template>
  <ZDialog v-model="open" :title="row ? t('settings.pricing.editModelPricing') : t('settings.pricing.addModelPricing')" width="42rem" :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving">
    <template #description>
      <span class="inline-flex items-center gap-2 align-middle text-cp-sm font-normal">
        <ProviderIconGroup :provider="provider" size="sm" />
        <span>{{ t('settings.pricing.unitLabel') }}</span>
      </span>
    </template>
    <div class="grid gap-5">
      <div class="grid grid-cols-[minmax(0,1fr)_6.5rem] gap-3 sm:grid-cols-[minmax(0,1fr)_8rem] sm:gap-4">
        <ZFormItem :label="t('settings.pricing.upstreamModelId')">
          <ZInput v-model="model" :disabled="saving || !!row" :aria-label="t('settings.pricing.upstreamModelId')" :placeholder="t('settings.pricing.forExampleGpt')" class="font-mono" />
        </ZFormItem>
        <ZFormItem :label="t('settings.pricing.multiplier')">
          <ZInput v-model="multiplier" :aria-label="t('settings.pricing.customizedMultiplier')" class="font-mono" inputmode="decimal" :disabled="saving">
            <template #suffix>
              ×
            </template>
          </ZInput>
        </ZFormItem>
      </div>
      <section :aria-label="t('settings.pricing.gearPrice')">
        <div class="mb-5 flex flex-wrap items-center justify-between gap-3">
          <ZSelect :model-value="selectedBand" :options="bandOptions" :aria-label="t('settings.pricing.priceRange')" class="min-w-0 flex-1 sm:max-w-56" :disabled="saving" @update:model-value="selectedBand = $event as PriceBand" />
          <ZSegmented v-model="priceMode" :aria-label="t('settings.pricing.unitPriceSource')" :options="sourceOptions" :disabled="saving" />
        </div>
        <PricingRateFields v-if="current || base" class="rounded-cp-lg bg-cp-fill-quaternary p-4" :base="base" :custom="current" :multiplier-bps="bps" :disabled="saving" @change="updatePrice" />
        <div v-else class="grid justify-items-center gap-3 py-6 text-cp-sm text-cp-text-tertiary">
          <p class="m-0">
            {{ t('settings.pricing.tier.sourceUnavailable') }}
          </p>
          <ZButton size="small" :disabled="saving" @click="toggleBand(true)">
            {{ t('settings.pricing.fillInUnitPrice') }}
          </ZButton>
        </div>
      </section>
      <p v-if="validation" role="alert" class="m-0 rounded-cp bg-cp-error-container p-3 text-cp-sm text-cp-error-on-container">
        {{ validation }}
      </p>
    </div>
    <template #footer>
      <ZPopover v-model="helpOpen" class="mr-auto self-center" placement="top-start" trigger="hover-click">
        <template #reference>
          <ZButton variant="text" :aria-expanded="helpOpen" :aria-controls="helpOpen ? helpId : undefined">
            <template #icon>
              <Info class="size-3.5" aria-hidden="true" />
            </template>
            {{ t('settings.pricing.pricingInstructions') }}
          </ZButton>
        </template>
        <div :id="helpId" class="grid w-72 gap-2 p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
          <p class="m-0">
            {{ t('settings.pricing.modelIdUsesNameActuallySentTo') }}
          </p>
          <p class="m-0">
            {{ t('settings.pricing.customTierHelp') }}
          </p>
          <p class="m-0">
            {{ t('settings.pricing.estimateHelp') }}
          </p>
        </div>
      </ZPopover>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton><ZButton type="primary" :loading="saving" @click="submit">
        {{ t('common.actions.save') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
