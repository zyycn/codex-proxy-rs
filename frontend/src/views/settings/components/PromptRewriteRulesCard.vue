<script setup lang="ts">
import type { PromptRewriteRule } from '@/api'
import { ZButton, ZCard, ZEmpty, ZFormItem, ZIconButton, ZTextarea } from '@codex-proxy/ui'
import { Layers, LayoutGrid, Plus, Trash2 } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderSelect from '@/components/ProviderSelect.vue'
import { formatProviderLabel, isBuiltinProvider } from '@/providers/catalog'

defineProps<{ disabled: boolean, ready: boolean }>()
const rules = defineModel<PromptRewriteRule[]>({ required: true })
const { t } = useI18n({ useScope: 'global' })

const allPlatforms = computed(() => ({
  value: '',
  label: t('common.providers.all'),
  icon: LayoutGrid,
}))

function leadingOptions(providers: string[]) {
  if (!providers.length || (providers.length === 1 && isBuiltinProvider(providers[0])))
    return allPlatforms.value
  // API 保存的组合或自定义上游也需要显示当前选中范围
  return [
    allPlatforms.value,
    {
      value: providers.join(', '),
      label: providers.map(provider => formatProviderLabel(provider)).join(' + '),
      icon: Layers,
    },
  ]
}

function update<K extends keyof PromptRewriteRule>(index: number, key: K, value: PromptRewriteRule[K]) {
  rules.value = rules.value.map((rule, i) => i === index
    ? { ...rule, [key]: value }
    : rule)
}
</script>

<template>
  <ZCard
    class="@container"
    :title="t('settings.promptRewrite.title')"
    :description="t('settings.promptRewrite.description')"
  >
    <template #actions>
      <ZButton :disabled="disabled" @click="rules = [...rules, { providers: [], pattern: '', replacement: '' }]">
        <template #icon>
          <Plus class="size-4" />
        </template>
        {{ t('settings.errorProtection.addRule') }}
      </ZButton>
    </template>
    <template v-if="ready || rules.length" #body>
      <ZEmpty v-if="!rules.length" :title="t('settings.promptRewrite.empty')" size="small" surface="none" role="status" class="flex-1 content-center py-3!" />
      <div v-else class="grid gap-3">
        <div
          v-for="(rule, index) in rules" :key="index"
          class="grid grid-cols-1 items-start gap-3 rounded-z-card bg-z-fill-quaternary p-3 @xl:grid-cols-2"
        >
          <div class="col-span-full flex items-start justify-between gap-3">
            <ZFormItem class="min-w-0" :label="t('settings.promptRewrite.providers')">
              <ProviderSelect
                :model-value="rule.providers.join(', ')" :leading-option="leadingOptions(rule.providers)"
                :aria-label="t('settings.promptRewrite.providers')" :disabled="disabled"
                @update:model-value="update(index, 'providers', $event ? [$event] : [])"
              />
            </ZFormItem>
            <ZIconButton
              :aria-label="t('common.actions.delete')"
              :disabled="disabled"
              @click="rules = rules.filter((_, i) => i !== index)"
            >
              <Trash2 class="size-4 text-z-error" />
            </ZIconButton>
          </div>
          <ZFormItem :label="t('settings.promptRewrite.matchPrefix')" :prop="`promptRewriteRules.${index}.pattern`" :show-message="false" :rules="{ required: true, message: t('settings.promptRewrite.required') }" required>
            <ZTextarea :model-value="rule.pattern" :aria-label="t('settings.promptRewrite.matchPrefix')" :disabled="disabled" :rows="2" resize="vertical" @update:model-value="update(index, 'pattern', $event)" />
          </ZFormItem>
          <ZFormItem :label="t('settings.promptRewrite.replacement')">
            <ZTextarea :model-value="rule.replacement" :aria-label="t('settings.promptRewrite.replacement')" :disabled="disabled" :rows="2" resize="vertical" @update:model-value="update(index, 'replacement', $event)" />
          </ZFormItem>
        </div>
      </div>
    </template>
  </ZCard>
</template>
