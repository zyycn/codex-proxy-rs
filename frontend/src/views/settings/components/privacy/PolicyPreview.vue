<script setup lang="ts">
import type { CodexPrivacyPolicy, PrivacySample } from '@/api/modules/settings/privacy'
import { BaseButton, BaseFormItem, BaseModal, BaseTextarea } from '@codex-proxy/ui'
import { computed, shallowRef, watch } from 'vue'
import { usePrivacyPreview } from '../../composables/usePrivacyPreview'
import { exampleRequest } from './presets'

const props = defineProps<{ policy: CodexPrivacyPolicy }>()
const open = defineModel<boolean>({ required: true })
const sample = shallowRef('')
const localError = shallowRef('')
const preview = usePrivacyPreview()
const { loading, error, result } = preview
const output = computed(() => result.value ? JSON.stringify({ body: result.value.body, headers: result.value.headers, turnMetadata: result.value.turnMetadata }, null, 2) : '')
watch(open, (value) => {
  preview.invalidate()
  sample.value = value ? JSON.stringify(exampleRequest(), null, 2) : ''
  localError.value = ''
})
watch([sample, () => props.policy], () => preview.invalidate(), { deep: true, flush: 'sync' })
async function test() {
  localError.value = ''
  try {
    const parsed = JSON.parse(sample.value) as PrivacySample
    await preview.run({ ...props.policy, enabled: true }, parsed)
  }
  catch { localError.value = '请输入有效的请求样本 JSON' }
}
</script>

<template>
  <BaseModal v-model="open" title="测试规则" description="按草稿顺序执行已启用的规则，样本仅用于本次测试" size="lg">
    <div class="grid gap-3">
      <div class="grid min-w-0 gap-3 sm:grid-cols-2">
        <BaseFormItem label="请求样本">
          <BaseTextarea v-model="sample" aria-label="请求样本" placeholder="粘贴包含 body、headers、turnMetadata 的请求 JSON" resize="none" maxlength="262144" size="sm" class="font-mono [&_textarea]:block [&_textarea]:h-64" spellcheck="false" />
        </BaseFormItem>
        <div class="grid min-w-0 content-start gap-2">
          <span class="flex min-h-4 items-center text-cp leading-none font-medium text-cp-text-secondary">最终结果</span><pre class="cp-scrollbar m-0 h-64 overflow-auto rounded-cp bg-cp-fill-quaternary p-3 font-mono text-cp-xs break-all whitespace-pre-wrap">{{ output || '点击测试查看结果' }}</pre>
        </div>
      </div>
      <div v-if="result" class="grid gap-2">
        <div v-for="(outcome, index) in result.outcomes" :key="outcome.ruleId" class="flex flex-wrap justify-between gap-2 text-cp-sm">
          <span>{{ index + 1 }} · {{ policy.rules.find(rule => rule.id === outcome.ruleId)?.name }}</span>
          <span :class="outcome.status === 'skipped' ? 'text-cp-warning-text' : 'text-cp-text-secondary'">{{ outcome.reason || (outcome.status === 'disabled' ? '未启用' : `命中 ${outcome.matches} 处`) }}</span>
        </div>
      </div>
      <p v-if="localError || error" role="alert" class="m-0 text-cp-sm text-cp-error-text">
        {{ localError || error }}
      </p>
    </div>
    <template #footer>
      <BaseButton size="sm" variant="secondary" @click="open = false">
        关闭
      </BaseButton><BaseButton size="sm" variant="primary" :loading="loading" @click="test">
        测试规则
      </BaseButton>
    </template>
  </BaseModal>
</template>
