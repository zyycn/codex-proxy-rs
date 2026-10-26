<script setup lang="ts">
import type { CodexPrivacyPolicy, PrivacyRule } from '@/api/modules/settings/privacy'

import { ZButton, ZCard, ZIconButton, ZMenuItem, ZPopover, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { ChevronDown, CircleHelp, Play, Plus } from '@lucide/vue'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'
import PolicyPreview from './PolicyPreview.vue'
import { createPreset, newRule, presets } from './presets'
import RuleEditor from './RuleEditor.vue'
import RulesTable from './RulesTable.vue'

defineProps<{ disabled: boolean }>()

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<CodexPrivacyPolicy>({ required: true })
const editorOpen = shallowRef(false)
const selected = shallowRef<PrivacyRule | null>(null)
const isNew = shallowRef(false)
const presetOpen = shallowRef(false)
const testOpen = shallowRef(false)
const stepCount = computed(() => model.value.rules.reduce((count, rule) => count + rule.steps.length, 0))
const availableSteps = computed(() => 32 - stepCount.value + (isNew.value
  ? 0
  : selected.value?.steps.length ?? 0))
const failureOptions = computed<{ value: CodexPrivacyPolicy['onError'], label: string }[]>(() => ([
  {
    value: 'skip_rule',
    label: t('settings.privacy.skipCurrentRule'),
  },
  {
    value: 'reject_request',
    label: t('settings.privacy.blockRequestFromBeingSent'),
  },
]))

function edit(rule: PrivacyRule, create = false) {
  selected.value = rule
  isNew.value = create
  editorOpen.value = true
  presetOpen.value = false
}
function apply(rule: PrivacyRule) {
  model.value = {
    ...model.value,
    rules: isNew.value
      ? [...model.value.rules, rule]
      : model.value.rules.map(item => item.id === rule.id
          ? rule
          : item),
  }
}
function add(key?: string) {
  edit(key
    ? createPreset(key)
    : newRule(), true)
}
function toggle(rule: PrivacyRule, enabled: boolean) {
  model.value = {
    ...model.value,
    rules: model.value.rules.map(item => item.id === rule.id
      ? { ...item, enabled }
      : item),
  }
}
function remove(rule: PrivacyRule) {
  model.value = {
    ...model.value,
    rules: model.value.rules.filter(item => item.id !== rule.id),
  }
}
</script>

<template>
  <ZCard id="privacy-policy" :title="t('settings.privacy.privacyPolicy')" :description="t('settings.privacy.description')">
    <template #actions>
      <ZSwitch v-model="model.enabled" :aria-label="t('settings.privacy.enableRequestRules')" :active-text="t('common.values.on')" :inactive-text="t('common.values.off')" inline-prompt :width="56" :disabled="disabled" />
    </template>
    <template #body>
      <div class="grid gap-3">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div class="flex flex-wrap items-center gap-2">
            <ZButton size="small" :disabled="disabled || stepCount >= 32" @click="add()">
              <template #icon>
                <Plus class="size-3.5" />
              </template>{{ t('settings.privacy.addRule') }}
            </ZButton>
            <ZPopover v-model="presetOpen" placement="bottom-start">
              <template #reference>
                <ZButton size="small" variant="text" :disabled="disabled || stepCount >= 32">
                  {{ t('settings.privacy.addFromPreset') }}<ChevronDown class="ml-1 size-3" />
                </ZButton>
              </template>
              <div class="w-48 p-1.5">
                <ZMenuItem v-for="preset in presets" :key="preset.key" :disabled="stepCount + preset.steps.length > 32" @click="add(preset.key)">
                  {{ preset.name }}
                </ZMenuItem>
              </div>
            </ZPopover>
          </div>
          <ZButton size="small" variant="text" :disabled="disabled" @click="testOpen = true">
            <template #icon>
              <Play class="size-3.5" />
            </template>{{ t('settings.privacy.preview.testRules') }}
          </ZButton>
        </div>
        <RulesTable :rules="model.rules" :disabled="disabled" @edit="edit($event)" @toggle="toggle" @remove="remove" @reorder="model = { ...model, rules: $event }" />
        <div v-if="model.rules.length" class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
          <span class="text-cp-xs text-cp-text-quaternary">{{ t('settings.privacy.runsInListOrderRulesSteps', { count: model.rules.length, count2: stepCount }) }}</span>
          <div class="flex items-center gap-2">
            <span class="text-cp-xs text-cp-text-secondary">{{ t('settings.privacy.executionFailed') }}</span>
            <ZSelect v-model="model.onError" size="small" class="w-34" :options="failureOptions" :aria-label="t('settings.privacy.ruleExecutionFailed')" :disabled="disabled" />
            <ZPopover trigger="hover-click" placement="top-end">
              <template #reference="{ open: helpOpen }">
                <ZIconButton size="small" :aria-label="t('settings.privacy.ruleExecutionInstructions')" :title="undefined" class="hover:bg-transparent! active:bg-transparent!" :aria-expanded="helpOpen">
                  <CircleHelp class="size-3.5" />
                </ZIconButton>
              </template>
              <div class="max-w-72 space-y-2 p-3 text-cp-sm text-cp-text-secondary">
                <p class="m-0">
                  {{ t('settings.privacy.skipHelp') }}
                </p><p class="m-0">
                  {{ t('settings.privacy.overrideHelp') }}
                </p>
              </div>
            </ZPopover>
          </div>
        </div>
      </div>
    </template>
  </ZCard>
  <RuleEditor v-model="editorOpen" :rule="selected" :max-steps="availableSteps" :is-new="isNew" :disabled="disabled" @apply="apply" />
  <PolicyPreview v-model="testOpen" :policy="model" />
</template>
