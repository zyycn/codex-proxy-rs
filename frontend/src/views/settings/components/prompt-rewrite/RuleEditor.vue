<script setup lang="ts">
import type { PromptRewriteRule } from '@/api'
import { ZButton, ZDialog, ZForm, ZFormItem, ZInput, ZTextarea } from '@codex-proxy/ui'
import { Layers, LayoutGrid } from '@lucide/vue'
import { computed, ref, useId, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import ProviderSelect from '@/components/ProviderSelect.vue'
import { formatProviderLabel, isBuiltinProvider } from '@/providers/catalog'

const props = defineProps<{ rule?: PromptRewriteRule, disabled: boolean }>()
const emit = defineEmits<{ confirm: [rule: PromptRewriteRule] }>()
const open = defineModel<boolean>({ required: true })
const { t } = useI18n({ useScope: 'global' })
const formId = useId()
const formRef = useTemplateRef<InstanceType<typeof ZForm>>('formElement')
const draft = ref<PromptRewriteRule>({
  name: '',
  providers: [],
  pattern: '',
  replacement: '',
})
const leadingOptions = computed(() => {
  const all = {
    value: '',
    label: t('common.providers.all'),
    icon: LayoutGrid,
  }
  const providers = draft.value.providers
  if (!providers.length || (providers.length === 1 && isBuiltinProvider(providers[0])))
    return all
  // 已保存的组合或自定义上游继续使用原范围
  return [
    all,
    {
      value: providers.join(', '),
      label: providers.map(provider => formatProviderLabel(provider)).join(' + '),
      icon: Layers,
    },
  ]
})

watch(open, (value) => {
  if (value) {
    draft.value = {
      name: props.rule?.name ?? '',
      providers: [
        ...(props.rule?.providers ?? []),
      ],
      pattern: props.rule?.pattern ?? '',
      replacement: props.rule?.replacement ?? '',
    }
    formRef.value?.clearValidate()
  }
}, { immediate: true })

async function confirm() {
  if (props.disabled || !await formRef.value?.validate().catch(() => false) || !open.value)
    return
  emit('confirm', {
    ...draft.value,
    name: draft.value.name.trim(),
  })
  open.value = false
}
</script>

<template>
  <ZDialog v-model="open" destroy-on-close :title="rule ? t('settings.promptRewrite.editRule') : t('settings.promptRewrite.addRule')" width="52rem">
    <ZForm :id="formId" ref="formElement" :model="draft" novalidate @submit.stop.prevent="confirm">
      <ZFormItem :label="t('settings.promptRewrite.ruleName')" prop="name" :show-message="false" :rules="{ required: true, whitespace: true, message: t('settings.promptRewrite.nameRequired') }" required>
        <ZInput v-model="draft.name" :aria-label="t('settings.promptRewrite.ruleName')" :placeholder="t('settings.promptRewrite.namePlaceholder')" :disabled="disabled" />
      </ZFormItem>
      <ZFormItem :label="t('settings.promptRewrite.providers')">
        <ProviderSelect
          :model-value="draft.providers.join(', ')" :leading-option="leadingOptions"
          :aria-label="t('settings.promptRewrite.providers')" :disabled="disabled"
          @update:model-value="draft.providers = $event ? [$event] : []"
        />
      </ZFormItem>
      <div class="grid min-w-0 gap-4 sm:grid-cols-2">
        <ZFormItem :label="t('settings.promptRewrite.matchPrefix')" prop="pattern" :show-message="false" :rules="{ required: true, message: t('settings.promptRewrite.required') }" required>
          <ZTextarea v-model="draft.pattern" :aria-label="t('settings.promptRewrite.matchPrefix')" :disabled="disabled" :rows="8" resize="vertical" />
        </ZFormItem>
        <ZFormItem :label="t('settings.promptRewrite.replacement')">
          <ZTextarea v-model="draft.replacement" :aria-label="t('settings.promptRewrite.replacement')" :disabled="disabled" :rows="8" resize="vertical" />
        </ZFormItem>
      </div>
    </ZForm>
    <template #footer>
      <ZButton @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton native-type="submit" :form="formId" type="primary" :disabled="disabled">
        {{ t('common.actions.confirm') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
