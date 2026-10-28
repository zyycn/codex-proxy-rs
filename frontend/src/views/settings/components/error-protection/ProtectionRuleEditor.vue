<script setup lang="ts">
import type { ProtectionCondition, ProtectionRule } from '@/api/modules/settings/protection'
import { ZButton } from '@codex-proxy/ui'
import { Plus } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { createProtectionCondition, protectionLimits, protectionRuleStructureError } from './form'
import ProtectionConditionEditor from './ProtectionConditionEditor.vue'

const props = defineProps<{ disabled: boolean }>()
const rule = defineModel<ProtectionRule>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const error = computed(() => protectionRuleStructureError(rule.value))

function update(patch: Partial<ProtectionRule>) {
  if (!props.disabled) {
    rule.value = {
      ...rule.value,
      ...patch,
    }
  }
}

function updateCondition(index: number, condition: ProtectionCondition) {
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
      <span class="text-cp-sm font-emphasis text-cp-text-secondary">{{ t('settings.errorProtection.allConditions') }}</span>
      <template v-for="(condition, index) in rule.conditions" :key="index">
        <span v-if="index > 0" class="pl-3 text-cp-xs font-emphasis text-cp-text-secondary">{{ t('settings.errorProtection.and') }}</span>
        <ProtectionConditionEditor
          :model-value="condition"
          :disabled="disabled"
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
    <p v-if="error" class="m-0 text-cp-sm text-cp-error-text" role="alert">
      {{ error }}
    </p>
  </div>
</template>
