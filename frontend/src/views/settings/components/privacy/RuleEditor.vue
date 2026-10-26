<script setup lang="ts">
import type { PrivacyRule, PrivacySample, PrivacyStep } from '@/api/modules/settings/privacy'

import { ZButton, ZDialog, ZFormItem, ZInput, ZTextarea } from '@codex-proxy/ui'
import { Play } from '@lucide/vue'
import { cloneDeep } from 'es-toolkit'
import { computed, ref, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { usePrivacyPreview } from '../../composables/usePrivacyPreview'
import { exampleRequest, newRule, newStep } from './presets'
import StepFields from './StepFields.vue'
import StepList from './StepList.vue'

const props = defineProps<{ rule: PrivacyRule | null, maxSteps: number, isNew: boolean, disabled: boolean }>()

const emit = defineEmits<{ apply: [rule: PrivacyRule] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const draft = ref<PrivacyRule>(newRule())
const selectedIndex = shallowRef(0)
const step = computed({
  get: () => draft.value.steps[selectedIndex.value]!,
  set: value => draft.value.steps[selectedIndex.value] = value,
})
const sample = shallowRef('')
const fixedValues = ref<string[]>(['""'])
const fixedValue = computed({
  get: () => fixedValues.value[selectedIndex.value] ?? '""',
  set: (value: string) => {
    fixedValues.value[selectedIndex.value] = value
  },
})
const localError = shallowRef('')
const preview = usePrivacyPreview()
const { loading, result } = preview
const processed = computed(() => result.value
  ? JSON.stringify({ body: result.value.body, headers: result.value.headers, turnMetadata: result.value.turnMetadata }, null, 2)
  : '')
const outcome = computed(() => result.value?.outcomes[0])

watch(open, (value) => {
  preview.invalidate()
  localError.value = ''
  if (value) {
    selectedIndex.value = 0
    draft.value = cloneDeep(props.rule ?? newRule())
    fixedValues.value = draft.value.steps.map(step => JSON.stringify(step.value))
    sample.value = JSON.stringify(exampleRequest(), null, 2)
  }
  else {
    sample.value = ''
  }
})
watch([draft, sample, fixedValues], () => {
  preview.invalidate()
  localError.value = ''
}, { deep: true, flush: 'sync' })

function addStep() {
  if (draft.value.steps.length >= props.maxSteps)
    return
  draft.value.steps.push(newStep())
  fixedValues.value.push('""')
  selectedIndex.value = draft.value.steps.length - 1
}
function removeStep(index: number) {
  if (draft.value.steps.length <= 1)
    return
  if (index <= selectedIndex.value)
    selectedIndex.value = Math.max(0, selectedIndex.value - 1)
  draft.value.steps.splice(index, 1)
  fixedValues.value.splice(index, 1)
}
function reorderSteps(steps: PrivacyStep[]) {
  // JSON 草稿和当前编辑项随步骤一起移动，避免排序后串到其他步骤
  const order = steps.map(step => draft.value.steps.indexOf(step))
  const nextSelected = order.indexOf(selectedIndex.value)
  fixedValues.value = order.map(index => fixedValues.value[index]!)
  draft.value.steps = steps
  selectedIndex.value = nextSelected
}
function preparedRule() {
  const rule = cloneDeep(draft.value)
  if (!rule.name.trim())
    throw new Error(t('settings.privacy.rules.fillInRuleName'))
  if (rule.steps.length > props.maxSteps)
    throw new Error(t('settings.privacy.rules.stepLimit'))
  rule.steps.forEach((step, index) => {
    if (!step.selector.trim())
      throw new Error(t('settings.privacy.rules.enterFieldPathForStep', { value: index + 1 }))
    if (step.action === 'set_value') {
      try {
        step.value = JSON.parse(fixedValues.value[index] ?? '""')
      }
      catch { throw new Error(t('settings.privacy.rules.fixedValueForStepMustBeJson', { value: index + 1 })) }
    }
  })
  return rule
}
async function test() {
  try {
    const rule = preparedRule()
    const value: PrivacySample = JSON.parse(sample.value)
    await preview.run({
      enabled: true,
      onError: 'skip_rule',
      rules: [
        { ...rule, enabled: true },
      ],
    }, value)
  }
  catch (cause) {
    localError.value = cause instanceof Error
      ? cause.message
      : t('settings.privacy.rules.invalidSampleFormat')
  }
}
async function apply() {
  if (props.disabled)
    return
  try {
    const rule = preparedRule()
    const validated = await preview.run({
      enabled: false,
      onError: 'skip_rule',
      rules: [rule],
    }, { body: {}, headers: {}, turnMetadata: null })
    if (!validated || !open.value)
      return
    emit('apply', rule)
    open.value = false
  }
  catch (cause) {
    localError.value = cause instanceof Error
      ? cause.message
      : t('settings.privacy.rules.ruleFormatIsInvalid')
  }
}
</script>

<template>
  <ZDialog v-model="open" :title="isNew ? t('settings.privacy.addRule') : t('settings.privacy.rules.editRule')" :description="t('settings.privacy.rules.description')" width="64rem">
    <div class="grid gap-4">
      <ZFormItem :label="t('settings.privacy.rules.ruleName')" required>
        <ZInput v-model="draft.name" :placeholder="t('settings.privacy.rules.namePlaceholder')" :aria-label="t('settings.privacy.rules.ruleName')" maxlength="64" />
      </ZFormItem>
      <div class="grid min-w-0 gap-5 md:h-128 md:grid-cols-[18rem_minmax(0,1fr)]">
        <StepList v-model="selectedIndex" :steps="draft.steps" :fixed-values="fixedValues" :can-add="draft.steps.length < maxSteps" @add="addStep" @remove="removeStep" @reorder="reorderSteps" />
        <!-- 表单按内容排布，预览文本域填充余下高度，切换步骤不会撑动列表或弹窗 -->
        <div class="grid h-224 min-h-0 min-w-0 grid-rows-[auto_minmax(0,1fr)] gap-5 sm:h-128 md:h-auto">
          <section class="min-w-0" :aria-label="t('settings.privacy.rules.currentStepSettings')">
            <StepFields v-model="step" v-model:fixed-value="fixedValue" />
          </section>
          <section class="flex min-h-0 flex-col gap-2.5" :aria-label="t('settings.privacy.rules.testPreview')">
            <div class="flex shrink-0 items-center justify-between gap-3">
              <h3 class="m-0 text-cp-sm font-emphasis">
                {{ t('settings.privacy.rules.previewOfEntireRule') }}
              </h3>
              <div class="flex items-center gap-2">
                <span v-if="outcome" class="text-cp-xs text-cp-text-quaternary">{{ outcome.status === 'skipped' ? t('settings.privacy.rules.ruleSkipped') : t('settings.privacy.rules.matches', { matches: outcome.matches }) }}</span>
                <ZButton size="small" variant="text" :loading="loading" :aria-label="t('settings.privacy.rules.testThisRule')" @click="test">
                  <template #icon>
                    <Play class="size-3.5" />
                  </template>{{ t('settings.privacy.rules.test') }}
                </ZButton>
              </div>
            </div>
            <div class="grid min-h-0 min-w-0 flex-1 grid-rows-2 gap-3 sm:grid-cols-2 sm:grid-rows-1">
              <ZFormItem :label="t('settings.privacy.rules.testSample')" class="grid min-h-0 grid-rows-[auto_minmax(0,1fr)] [&>div:last-child]:min-h-0">
                <ZTextarea v-model="sample" size="small" resize="none" :placeholder="t('settings.privacy.rules.samplePlaceholder')" maxlength="262144" :aria-label="t('settings.privacy.rules.testSample')" class="h-full font-mono [&_textarea]:block [&_textarea]:h-full" spellcheck="false" />
              </ZFormItem>
              <div class="grid min-h-0 min-w-0 grid-rows-[auto_minmax(0,1fr)] gap-2">
                <span class="flex min-h-4 items-center text-cp leading-none font-medium text-cp-text-secondary">{{ t('settings.privacy.rules.afterProcessing') }}</span>
                <pre class="cp-scrollbar m-0 min-h-0 overflow-auto rounded-cp bg-cp-fill-quaternary p-3 font-mono text-cp-xs break-all whitespace-pre-wrap" data-preview-output>{{ processed || t('settings.privacy.testPlaceholder') }}</pre>
              </div>
            </div>
            <p v-if="localError || outcome?.reason" role="alert" class="m-0 shrink-0 text-cp-sm text-cp-error-text">
              {{ localError || outcome?.reason }}
            </p>
          </section>
        </div>
      </div>
    </div>
    <template #footer>
      <ZButton @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :loading="loading" :disabled="disabled" @click="apply">
        {{ t('settings.privacy.rules.applyToDraft') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
