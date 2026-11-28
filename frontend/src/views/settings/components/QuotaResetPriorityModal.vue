<script setup lang="ts">
import type { ResetWindowWeights } from '@/api'
import { ZButton, ZDialog, ZForm } from '@codex-proxy/ui'
import { ref, shallowRef, useId, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import ResetWindowWeightsFields from './ResetWindowWeightsFields.vue'

const props = defineProps<{
  config: ResetWindowWeights
  defaults: ResetWindowWeights
}>()
const emit = defineEmits<{ confirm: [config: ResetWindowWeights] }>()
const open = defineModel<boolean>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const formId = useId()
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const draft = ref({ fiveHour: '', weekly: '', monthly: '' })
const submitted = shallowRef(false)

function replaceDraft(config: ResetWindowWeights) {
  for (const key of Object.keys(draft.value) as (keyof ResetWindowWeights)[])
    draft.value[key] = String(config[key])
  submitted.value = false
  formRef.value?.clearValidate()
}

watch(open, (value) => {
  if (value)
    replaceDraft(props.config)
}, { immediate: true })

async function confirm() {
  submitted.value = true
  if (!await formRef.value?.validate().catch(() => false))
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
  <ZDialog v-model="open" destroy-on-close :title="t('settings.rotation.quotaResetPrioritySettings')" :description="t('settings.resetPriority.description')" width="36rem">
    <ZForm :id="formId" ref="formElement" class="gap-5" :model="{ resetWindowWeights: draft }" novalidate @submit="confirm">
      <ResetWindowWeightsFields v-model="draft" :submitted="submitted" :help="t('settings.resetPriority.windowHelp')" />
    </ZForm>
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
