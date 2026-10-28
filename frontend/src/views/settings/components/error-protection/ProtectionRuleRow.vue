<script setup lang="ts">
import type { ProtectionRule } from '@/api/modules/settings/protection'
import { ZIconButton, ZInput } from '@codex-proxy/ui'
import { ChevronDown, Trash2 } from '@lucide/vue'
import { computed, useId } from 'vue'
import { useI18n } from 'vue-i18n'
import { protectionRuleError, protectionRuleSummary } from './form'
import ProtectionRuleEditor from './ProtectionRuleEditor.vue'

const props = defineProps<{ rule: ProtectionRule, expanded: boolean, disabled: boolean }>()
const emit = defineEmits<{ toggle: [], remove: [], update: [rule: ProtectionRule] }>()
const { t } = useI18n({ useScope: 'global' })
const editorId = useId()
// 折叠行独立缓存摘要，输入时只更新当前规则的条件
const summary = computed(() => props.expanded
  ? ''
  : protectionRuleSummary(props.rule))
const error = computed(() => props.expanded
  ? ''
  : protectionRuleError(props.rule))

function updateName(name: string) {
  if (!props.disabled)
    emit('update', { ...props.rule, name })
}
</script>

<template>
  <div class="min-w-0 rounded-cp bg-cp-fill-quaternary">
    <div class="flex min-w-0 items-center gap-2 p-3 sm:px-4">
      <ZIconButton
        :aria-label="t('settings.errorProtection.editRule', { name: rule.name || t('settings.errorProtection.unnamedRule') })"
        :aria-expanded="expanded"
        :aria-controls="editorId"
        @click="emit('toggle')"
      >
        <ChevronDown class="size-4 transition-transform motion-reduce:transition-none" :class="expanded ? 'rotate-180' : undefined" />
      </ZIconButton>
      <div class="grid min-w-0 flex-1 gap-0.5">
        <ZInput
          v-if="expanded"
          :model-value="rule.name"
          :disabled="disabled"
          :aria-label="t('settings.errorProtection.ruleName')"
          :placeholder="t('settings.errorProtection.ruleNamePlaceholder')"
          class="max-w-lg"
          @update:model-value="updateName"
        />
        <span v-else class="truncate text-cp-sm font-emphasis text-cp-text">{{ rule.name || t('settings.errorProtection.unnamedRule') }}</span>
        <span v-if="!expanded" class="truncate text-cp-xs text-cp-text-tertiary" :title="summary">{{ summary }}</span>
        <span v-if="error" role="alert" class="text-cp-sm text-cp-error-text">{{ error }}</span>
      </div>
      <ZIconButton :disabled="disabled" :aria-label="t('settings.errorProtection.removeRule', { name: rule.name || t('settings.errorProtection.unnamedRule') })" @click="emit('remove')">
        <Trash2 class="size-4" />
      </ZIconButton>
    </div>
    <ProtectionRuleEditor v-if="expanded" :id="editorId" :model-value="rule" :disabled="disabled" @update:model-value="emit('update', $event)" />
  </div>
</template>
