<script setup lang="ts">
import type { PrivacyStep } from '@/api/modules/settings/privacy'

import { ZCheckbox, ZForm, ZFormItem, ZIconButton, ZInput, ZPopover, ZSelect, ZTextarea } from '@codex-proxy/ui'
import { CircleHelp } from '@lucide/vue'
import { computed, useId, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { actions, fieldsByScope, scopes } from './presets'

const { t } = useI18n({ useScope: 'global' })

const step = defineModel<PrivacyStep>({ required: true })
const fixedValue = defineModel<string>('fixedValue', { required: true })
const fieldPresetId = useId()
const usesPattern = computed(() => ['regex_replace', 'rename_key'].includes(step.value.action) || (step.value.action === 'remove_field' && step.value.pattern !== null))
const pattern = computed({
  get: () => step.value.pattern ?? '',
  set: (value: string) => {
    step.value.pattern = value
  },
})
const conditional = computed({
  get: () => step.value.pattern !== null,
  set: (value: boolean) => {
    step.value.pattern = value ? '' : null
  },
})
const fields = computed(() => fieldsByScope.value[step.value.scope] ?? [])
const fieldChoice = computed({
  get: () => fields.value.some(field => field.value === step.value.selector)
    ? step.value.selector
    : '',
  set: (value: string) => {
    step.value.selector = value
  },
})
const selectorPlaceholder = computed(() => ({
  turn_metadata: t('settings.privacy.steps.pathPlaceholder'),
  desktop_git_context: t('settings.privacy.steps.forExampleRemotesFetchurl'),
  environment_text: t('settings.privacy.steps.environmentPathHelp'),
  request_body: t('settings.privacy.steps.forExampleMetadataWorkspace'),
  request_header: t('settings.privacy.steps.forExampleXClientName'),
})[step.value.scope])

watch(() => step.value.action, (action) => {
  if (action === 'set_value')
    step.value.pattern = null
  else if (action !== 'remove_field' && step.value.pattern === null)
    step.value.pattern = ''
})
</script>

<template>
  <ZForm class="sm:grid-cols-2">
    <ZFormItem class="md:[&>div:first-child]:min-h-8" :label="t('settings.privacy.rules.scope')">
      <ZSelect v-model="step.scope" class="w-full" :options="scopes" :aria-label="t('settings.privacy.rules.scope')" />
    </ZFormItem>
    <ZFormItem class="md:[&>div:first-child]:min-h-8" :label="t('settings.privacy.rules.processingMethod')">
      <ZSelect v-model="step.action" class="w-full" :options="actions" :aria-label="t('settings.privacy.rules.processingMethod')" />
    </ZFormItem>
    <ZFormItem class="sm:col-span-2" :label="step.scope === 'request_header' ? t('settings.privacy.steps.requestHeaderName') : t('settings.privacy.steps.fieldPath')" required>
      <template #label-extra>
        <ZPopover trigger="hover-click">
          <template #reference="{ open: helpOpen }">
            <ZIconButton size="small" :aria-label="t('settings.privacy.steps.fieldPathSyntax')" :title="undefined" class="-my-2 hover:bg-transparent! active:bg-transparent!" :aria-expanded="helpOpen">
              <CircleHelp class="size-3.5" />
            </ZIconButton>
          </template>
          <div class="max-w-72 p-3 text-cp-sm text-cp-text-secondary">
            {{ t('settings.privacy.steps.jsonPathHelp') }}<br>{{ t('settings.privacy.steps.targetHelp') }}
          </div>
        </ZPopover>
      </template>
      <div class="flex flex-wrap items-center gap-2 sm:flex-nowrap">
        <ZInput v-model="step.selector" :placeholder="selectorPlaceholder" :aria-label="t('settings.privacy.steps.fieldPath')" maxlength="1024" class="min-w-0 flex-1 font-mono" :class="fields.length ? 'basis-full sm:basis-auto' : undefined" spellcheck="false" />
        <ZSelect v-if="fields.length" :id="fieldPresetId" v-model="fieldChoice" class="min-w-0 flex-1 sm:w-40 sm:flex-none" :options="fields" :aria-label="t('settings.privacy.steps.commonFields')" />
      </div>
    </ZFormItem>
    <ZCheckbox v-if="step.action === 'remove_field'" v-model="conditional" class="sm:col-span-2" :label="t('settings.privacy.steps.deleteMatchHelp')" />
    <ZFormItem v-if="usesPattern" :class="step.action === 'remove_field' ? 'sm:col-span-2' : undefined" :label="t('settings.privacy.steps.matchExpression')">
      <ZInput v-model="pattern" :placeholder="step.action === 'rename_key' ? t('settings.privacy.steps.forExampleHome') : t('settings.privacy.steps.forExamplePrivateOrg')" :aria-label="t('settings.privacy.steps.matchExpression')" maxlength="4096" spellcheck="false" class="font-mono" />
    </ZFormItem>
    <ZFormItem v-if="step.action === 'regex_replace' || step.action === 'rename_key'" :label="t('settings.privacy.steps.replaceContent')">
      <template #label-extra>
        <ZPopover trigger="hover-click">
          <template #reference="{ open: helpOpen }">
            <ZIconButton size="small" :aria-label="t('settings.privacy.steps.regularSyntaxDescription')" :title="undefined" class="-my-2 hover:bg-transparent! active:bg-transparent!" :aria-expanded="helpOpen">
              <CircleHelp class="size-3.5" />
            </ZIconButton>
          </template>
          <div class="max-w-72 p-3 text-cp-sm text-cp-text-secondary">
            {{ t('settings.privacy.steps.useOrForCaptureGroupsAndFor') }}<br>{{ t('settings.privacy.steps.usesRustRegexSyntaxAndDoesNot') }}
          </div>
        </ZPopover>
      </template>
      <ZInput v-model="step.replacement" :placeholder="step.action === 'rename_key' ? t('settings.privacy.steps.forExampleHomeUser') : t('settings.privacy.steps.forExampleTeamOrLeaveBlankTo')" :aria-label="t('settings.privacy.steps.replaceContent')" maxlength="4096" spellcheck="false" class="font-mono" />
    </ZFormItem>
    <ZFormItem v-if="step.action === 'set_value'" class="sm:col-span-2" :label="t('settings.privacy.steps.fixedValueJson')">
      <ZTextarea v-model="fixedValue" :placeholder="t('settings.privacy.steps.forExampleAnonymousNullOr')" :rows="2" resize="none" maxlength="4096" :aria-label="t('settings.privacy.steps.fixedJsonLabel')" class="font-mono" />
    </ZFormItem>
    <div v-if="usesPattern" class="flex flex-wrap gap-x-4 gap-y-2 sm:col-span-2">
      <ZCheckbox v-if="step.action !== 'remove_field'" v-model="step.replaceAll" :label="t('settings.privacy.steps.replaceAllMatches')" />
      <ZCheckbox v-model="step.caseInsensitive" :label="t('settings.privacy.steps.ignoreCase')" />
      <ZCheckbox v-model="step.multiLine" :label="t('settings.privacy.steps.matchMultipleLines')" />
    </div>
  </ZForm>
</template>
