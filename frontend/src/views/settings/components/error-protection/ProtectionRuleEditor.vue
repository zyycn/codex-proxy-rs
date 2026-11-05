<script setup lang="ts">
import type { ProtectionCondition, ProtectionRule } from '@/api/modules/settings/protection'
import { ZButton } from '@codex-proxy/ui'
import { Plus } from '@lucide/vue'
import { computed, toRaw } from 'vue'
import { useI18n } from 'vue-i18n'
import { createProtectionCondition, protectionLimits, protectionRuleConditionsError } from './form'
import ProtectionConditionEditor from './ProtectionConditionEditor.vue'

const props = defineProps<{ disabled: boolean, showErrors: boolean }>()
const rule = defineModel<ProtectionRule>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const error = computed(() => props.showErrors
  ? protectionRuleConditionsError(rule.value.conditions)
  : '')
const conditionKeys = new WeakMap<ProtectionCondition, number>()
let nextConditionKey = 0

function conditionKey(condition: ProtectionCondition) {
  const value = toRaw(condition)
  if (!conditionKeys.has(value))
    conditionKeys.set(value, nextConditionKey++)
  return conditionKeys.get(value)!
}

function update(patch: Partial<ProtectionRule>) {
  if (!props.disabled) {
    rule.value = {
      ...rule.value,
      ...patch,
    }
  }
}

function updateCondition(index: number, condition: ProtectionCondition) {
  // 对象更新沿用编辑身份，删除前面的条件时也不串用后续条件的输入状态
  conditionKeys.set(toRaw(condition), conditionKey(rule.value.conditions[index]!))
  update({
    conditions: rule.value.conditions.map((previous, i) => i === index
      ? condition
      : previous),
  })
}
</script>

<template>
  <div class="grid gap-4 px-3 pb-4 sm:px-4">
    <div class="grid gap-2">
      <span class="text-z-sm font-emphasis text-z-text-secondary">{{ t('settings.errorProtection.allConditions') }}</span>
      <template v-for="(condition, index) in rule.conditions" :key="conditionKey(condition)">
        <span v-if="index > 0" class="pl-3 text-z-xs font-emphasis text-z-text-secondary">{{ t('settings.errorProtection.and') }}</span>
        <ProtectionConditionEditor
          :model-value="condition"
          :disabled="disabled"
          :show-errors="showErrors"
          @update:model-value="updateCondition(index, $event)"
          @remove="update({ conditions: rule.conditions.filter((_, i) => i !== index) })"
        />
      </template>
      <ZButton
        class="justify-self-start"
        size="small"
        variant="text"
        :disabled="disabled || rule.conditions.length >= protectionLimits.conditions"
        @click="update({ conditions: [...rule.conditions, createProtectionCondition()] })"
      >
        <template #icon>
          <Plus class="size-3.5" />
        </template>
        {{ t('settings.errorProtection.addCondition') }}
      </ZButton>
    </div>
    <p v-if="error" class="m-0 text-z-sm text-z-error-text" role="alert">
      {{ error }}
    </p>
  </div>
</template>
