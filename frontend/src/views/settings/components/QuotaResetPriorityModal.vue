<script setup lang="ts">
import type { ResetWindowWeights } from '@/api'
import { ZButton, ZDialog } from '@codex-proxy/ui'
import { ref, shallowRef, useId, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { validSchedulingWeight } from '../utils/scheduling'
import ResetWindowWeightsFields from './ResetWindowWeightsFields.vue'

const props = defineProps<{
  config: ResetWindowWeights
  defaults: ResetWindowWeights
}>()
const emit = defineEmits<{ confirm: [config: ResetWindowWeights] }>()
const open = defineModel<boolean>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const formId = useId()
const draft = ref({ fiveHour: '', weekly: '', monthly: '' })
const submitted = shallowRef(false)

function replaceDraft(config: ResetWindowWeights) {
  for (const key of Object.keys(draft.value) as (keyof ResetWindowWeights)[])
    draft.value[key] = String(config[key])
  submitted.value = false
}

watch(open, (value) => {
  if (value)
    replaceDraft(props.config)
}, { immediate: true })

function confirm() {
  submitted.value = true
  if (!Object.values(draft.value).every(validSchedulingWeight))
    return
  emit('confirm', {
    fiveHour: Number(draft.value.fiveHour),
    weekly: Number(draft.value.weekly),
    monthly: Number(draft.value.monthly),
  })
  open.value = false
}
</script>

<template>
  <ZDialog v-model="open" :title="t('settings.rotation.quotaResetPrioritySettings')" :description="t('settings.resetPriority.description')" width="36rem">
    <form :id="formId" class="grid gap-5" novalidate @submit.prevent="confirm">
      <ResetWindowWeightsFields v-model="draft" :submitted="submitted" :help="t('settings.resetPriority.windowHelp')" />
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
