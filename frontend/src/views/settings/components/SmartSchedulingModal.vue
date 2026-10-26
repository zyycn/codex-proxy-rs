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
const fields = computed(() => ([
  {
    key: 'loadWeight',
    label: t('settings.scheduling.weights.load'),
    help: t('settings.scheduling.weights.loadHelp'),
  },
  {
    key: 'quotaWeight',
    label: t('settings.scheduling.weights.quota'),
    help: t('settings.scheduling.weights.quotaHelp'),
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
  {
    key: 'resetWeight',
    label: t('settings.scheduling.weights.reset'),
    help: t('settings.scheduling.weights.resetHelp'),
  },
  {
    key: 'queueWeight',
    label: t('settings.scheduling.weights.queue'),
    help: t('settings.scheduling.weights.queueHelp'),
  },
] as const))
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
  const value = Number(draft[key])
  const valid = draft[key].trim() && Number.isFinite(value) && value >= 0 && value <= 10 && Math.round(value * 10) / 10 === value
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
  submitted.value = false
}

watch(open, (value) => {
  if (value)
    replaceDraft(props.config)
}, { immediate: true })

function confirm() {
  submitted.value = true
  if (Object.values(errors.value).some(Boolean) || emptyWeights.value)
    return
  emit('confirm', {
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
      <p v-if="!active" class="m-0 text-cp text-cp-text-secondary">
        {{ t('settings.scheduling.enabledHelp') }}
      </p>
      <div>
        <div class="grid gap-4 sm:grid-cols-2">
          <ZFormItem v-for="field in fields" :key="field.key" :error="submitted ? errors[field.key] : undefined">
            <template #label>
              <span class="inline-flex items-center gap-1">
                {{ field.label }}
                <ZPopover trigger="hover-click" placement="top-start">
                  <template #reference="{ open: helpOpen }">
                    <ZIconButton :aria-label="t('settings.scheduling.about', { label: field.label })" :title="undefined" :aria-expanded="helpOpen" class="size-6! hover:bg-transparent! active:bg-transparent!">
                      <CircleHelp class="size-3.5" />
                    </ZIconButton>
                  </template>
                  <p class="m-0 max-w-64 px-3 py-2 text-cp-sm leading-relaxed">{{ field.help }}</p>
                </ZPopover>
              </span>
            </template>
            <ZInput v-model="draft[field.key]" type="number" min="0" max="10" step="0.1" :placeholder="t('settings.scheduling.zeroWeightHelp')" :aria-label="field.label" />
          </ZFormItem>
        </div>
        <p v-if="submitted && emptyWeights" role="alert" class="mt-2 mb-0 text-cp-sm text-cp-danger">
          {{ t('settings.scheduling.weightsRequired') }}
        </p>
      </div>
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
