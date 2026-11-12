<script setup lang="ts">
import type { SmartSchedulingConfig } from '@/api'

import { ZButton, ZDialog, ZFormItem, ZIconButton, ZInput, ZPopover } from '@codex-proxy/ui'
import { CircleHelp } from '@lucide/vue'
import { computed, reactive, shallowRef, useId, watch } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  config: SmartSchedulingConfig
  defaults: SmartSchedulingConfig
  active: boolean
}>()

const emit = defineEmits<{ confirm: [config: SmartSchedulingConfig] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const formId = useId()
interface WeightGroup {
  id: 'runtime' | 'quota' | 'queue'
  title: string
  fields: { key: Exclude<keyof SmartSchedulingConfig, 'resetWindowWeights'>, label: string, help: string }[]
}
const groups = computed<WeightGroup[]>(() => [
  {
    id: 'runtime',
    title: t('settings.scheduling.runtimeGroup'),
    fields: [
      {
        key: 'loadWeight',
        label: t('settings.scheduling.weights.load'),
        help: t('settings.scheduling.weights.loadHelp'),
      },
      {
        key: 'healthWeight',
        label: t('settings.scheduling.weights.health'),
        help: t('settings.scheduling.weights.healthHelp'),
      },
      {
        key: 'latencyWeight',
        label: t('settings.scheduling.weights.latency'),
        help: t('settings.scheduling.weights.latencyHelp'),
      },
    ],
  },
  {
    id: 'quota',
    title: t('settings.scheduling.quotaGroup'),
    fields: [
      {
        key: 'quotaWeight',
        label: t('settings.scheduling.weights.quota'),
        help: t('settings.scheduling.weights.quotaHelp'),
      },
      {
        key: 'resetWeight',
        label: t('settings.scheduling.weights.reset'),
        help: t('settings.scheduling.weights.resetHelp'),
      },
    ],
  },
  {
    id: 'queue',
    title: t('settings.scheduling.queueGroup'),
    fields: [
      {
        key: 'queueWeight',
        label: t('settings.scheduling.weights.queue'),
        help: t('settings.scheduling.weights.queueHelp'),
      },
    ],
  },
])
const fields = computed(() => groups.value.flatMap(group => group.fields))
const periods = computed(() => [
  {
    key: 'fiveHour',
    label: t('settings.scheduling.fiveHour'),
  },
  {
    key: 'weekly',
    label: t('settings.scheduling.weekly'),
  },
  {
    key: 'monthly',
    label: t('settings.scheduling.monthly'),
  },
] as const)
const windows = reactive({ fiveHour: '', weekly: '', monthly: '' })
function validWeight(input: string) {
  const value = Number(input)
  return input.trim() !== '' && Number.isFinite(value) && value >= 0 && value <= 10 && Math.round(value * 10) / 10 === value
}
const draft = reactive({
  loadWeight: '',
  quotaWeight: '',
  healthWeight: '',
  latencyWeight: '',
  resetWeight: '',
  queueWeight: '',
})
const submitted = shallowRef(false)
const errors = computed(() => Object.fromEntries(fields.value.map(({ key }) => {
  const valid = validWeight(draft[key])
  return [
    key,
    valid
      ? undefined
      : t('settings.scheduling.enterValueFromToUpToOne'),
  ]
})))
const emptyWeights = computed(() => fields.value.every(({ key }) => Number(draft[key]) === 0))

function replaceDraft(config: SmartSchedulingConfig) {
  for (const { key } of fields.value)
    draft[key] = String(config[key])
  for (const { key } of periods.value)
    windows[key] = String(config.resetWindowWeights[key])
  submitted.value = false
}

watch(open, (value) => {
  if (value)
    replaceDraft(props.config)
}, { immediate: true })

function confirm() {
  submitted.value = true
  if (Object.values(errors.value).some(Boolean) || emptyWeights.value || !Object.values(windows).every(validWeight))
    return
  emit('confirm', {
    resetWindowWeights: {
      fiveHour: Number(windows.fiveHour),
      weekly: Number(windows.weekly),
      monthly: Number(windows.monthly),
    },
    loadWeight: Number(draft.loadWeight),
    quotaWeight: Number(draft.quotaWeight),
    healthWeight: Number(draft.healthWeight),
    latencyWeight: Number(draft.latencyWeight),
    resetWeight: Number(draft.resetWeight),
    queueWeight: Number(draft.queueWeight),
  })
  open.value = false
}
</script>

<template>
  <ZDialog v-model="open" :title="t('settings.rotation.smartSchedulingSettings')" :description="t('settings.scheduling.description')" width="36rem">
    <form :id="formId" class="grid gap-5" novalidate @submit.prevent="confirm">
      <p v-if="!active" class="m-0 text-z text-z-text-secondary">
        {{ t('settings.scheduling.enabledHelp') }}
      </p>
      <section v-for="group in groups" :key="group.id" class="grid gap-3">
        <h3 class="m-0 text-z font-emphasis">
          {{ group.title }}
        </h3>
        <div class="grid gap-4" :class="group.id === 'runtime' ? 'sm:grid-cols-3' : 'sm:grid-cols-2'">
          <ZFormItem v-for="field in group.fields" :key="field.key" :label="field.label" :error="submitted ? errors[field.key] : undefined">
            <template #label-extra>
              <ZPopover class="-my-1" trigger="hover-click" placement="top-start">
                <template #reference="{ open: helpOpen }">
                  <ZIconButton variant="link" :aria-label="t('settings.scheduling.about', { label: field.label })" :aria-expanded="helpOpen" class="size-6!">
                    <CircleHelp class="size-3.5" />
                  </ZIconButton>
                </template>
                <p class="m-0 max-w-64 px-3 py-2 text-z-sm leading-relaxed">
                  {{ field.help }}
                </p>
              </ZPopover>
            </template>
            <ZInput v-model="draft[field.key]" type="number" min="0" max="10" step="0.1" :placeholder="t('settings.scheduling.zeroWeightHelp')" :aria-label="field.label" />
          </ZFormItem>
        </div>
      </section>
      <section class="grid gap-3" :aria-label="t('settings.scheduling.windowMode')">
        <div class="flex items-center gap-1">
          <h3 class="m-0 text-z font-emphasis">
            {{ t('settings.scheduling.windowMode') }}
          </h3>
          <ZPopover trigger="hover-click" placement="top-start">
            <template #reference="{ open: helpOpen }">
              <ZIconButton variant="link" :aria-label="t('settings.scheduling.about', { label: t('settings.scheduling.windowMode') })" :aria-expanded="helpOpen" class="size-6!">
                <CircleHelp class="size-3.5" />
              </ZIconButton>
            </template>
            <p class="m-0 max-w-64 px-3 py-2 text-z-sm leading-relaxed">
              {{ t('settings.scheduling.windowHelp') }}
            </p>
          </ZPopover>
        </div>
        <div class="grid grid-cols-3 gap-3">
          <ZFormItem v-for="period in periods" :key="period.key" class="min-w-0" :label="period.label" :error="submitted && !validWeight(windows[period.key]) ? t('settings.scheduling.enterValueFromToUpToOne') : undefined">
            <ZInput v-model="windows[period.key]" type="number" min="0" max="10" step="0.1" :aria-label="period.label" :aria-invalid="submitted && !validWeight(windows[period.key])" />
          </ZFormItem>
        </div>
      </section>
      <p v-if="submitted && emptyWeights" role="alert" class="m-0 text-z-sm text-z-danger">
        {{ t('settings.scheduling.weightsRequired') }}
      </p>
    </form>
    <template #footer>
      <ZButton class="mr-auto" @click="replaceDraft(defaults)">
        {{ t('common.actions.reset') }}
      </ZButton>
      <ZButton @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton native-type="submit" :form="formId" type="primary">
        {{ t('common.actions.confirm') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
