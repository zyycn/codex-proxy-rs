<script setup lang="ts">
import type { PrivacyRule } from '@/api/modules/settings/privacy'
import { BaseButton, BaseCheckbox, BaseForm, BaseFormItem, BaseIconButton, BaseInput, BaseModal, BasePopover, BaseSelect, BaseTextarea } from '@codex-proxy/ui'
import { CircleHelp, Play } from '@lucide/vue'
import { cloneDeep } from 'es-toolkit'
import { computed, ref, shallowRef, useId, watch } from 'vue'
import { usePrivacyPreview } from '../../composables/usePrivacyPreview'
import { actions, fieldsByScope, makeSample, newRule, sampleText, scopes } from './presets'

const props = defineProps<{ rule: PrivacyRule | null, isNew: boolean, disabled: boolean }>()
const emit = defineEmits<{ apply: [rule: PrivacyRule] }>()
const open = defineModel<boolean>({ required: true })
const fieldPresetId = useId()
const draft = ref<PrivacyRule>(newRule())
const sample = shallowRef('')
const fixedValue = shallowRef('""')
const localError = shallowRef('')
const preview = usePrivacyPreview()
const { loading, error, result } = preview
const usesPattern = computed(() => ['regex_replace', 'rename_key'].includes(draft.value.action) || (draft.value.action === 'remove_field' && draft.value.pattern !== null))
const pattern = computed({ get: () => draft.value.pattern ?? '', set: (value: string) => {
  draft.value.pattern = value
} })
const conditional = computed({ get: () => draft.value.pattern !== null, set: (value: boolean) => {
  draft.value.pattern = value ? '' : null
} })
const fields = computed(() => fieldsByScope[draft.value.scope] ?? [])
const fieldChoice = computed({ get: () => fields.value.some(field => field.value === draft.value.selector) ? draft.value.selector : '', set: (value: string) => {
  draft.value.selector = value
} })
const selectorPlaceholder = computed(() => ({
  turn_metadata: '例如：$.workspaces.*.associated_remote_urls.*',
  desktop_git_context: '例如：$.remotes[*].fetchUrl',
  environment_text: '填写 $，匹配整段环境文本',
  request_body: '例如：$.metadata.workspace',
  request_header: '例如：x-client-name',
})[draft.value.scope])
const processed = computed(() => {
  if (!result.value)
    return ''
  if (draft.value.scope === 'turn_metadata')
    return JSON.stringify(JSON.parse(result.value.turnMetadata ?? '{}'), null, 2)
  if (draft.value.scope === 'request_header')
    return JSON.stringify(result.value.headers, null, 2)
  return JSON.stringify(result.value.body, null, 2)
})
const outcome = computed(() => result.value?.outcomes[0])

watch(open, (value) => {
  preview.invalidate()
  localError.value = ''
  if (value) {
    draft.value = cloneDeep(props.rule ?? newRule())
    fixedValue.value = JSON.stringify(draft.value.value)
    sample.value = sampleText(draft.value.scope)
  }
  else {
    sample.value = ''
  }
})
watch(() => draft.value.scope, (scope) => {
  sample.value = sampleText(scope)
})
watch(() => draft.value.action, (action) => {
  if (action === 'set_value')
    draft.value.pattern = null
  else if (action !== 'remove_field' && draft.value.pattern === null)
    draft.value.pattern = ''
})
watch([draft, sample, fixedValue], () => {
  preview.invalidate()
  localError.value = ''
}, { deep: true, flush: 'sync' })

function preparedRule() {
  const rule = cloneDeep(draft.value)
  if (!rule.name.trim() || !rule.selector.trim())
    throw new Error('请填写规则名称和字段路径')
  if (rule.action === 'set_value') {
    try {
      rule.value = JSON.parse(fixedValue.value)
    }
    catch { throw new Error('固定值应为 JSON，例如 "别名"、false 或 123') }
  }
  return rule
}

async function test() {
  try {
    const rule = preparedRule()
    const value = makeSample(rule.scope, sample.value, rule.selector)
    await preview.run({ enabled: true, onError: 'skip_rule', rules: [{ ...rule, enabled: true }] }, value)
  }
  catch (cause) { localError.value = cause instanceof Error ? cause.message : '样本格式无效' }
}

async function apply() {
  if (props.disabled)
    return
  try {
    const rule = preparedRule()
    const validated = await preview.run({ enabled: false, onError: 'skip_rule', rules: [rule] }, { body: {}, headers: {}, turnMetadata: null })
    if (!validated || !open.value)
      return
    emit('apply', rule)
    open.value = false
  }
  catch (cause) { localError.value = cause instanceof Error ? cause.message : '规则格式无效' }
}
</script>

<template>
  <BaseModal v-model="open" :title="isNew ? '添加规则' : '编辑规则'" size="md-wide">
    <div class="grid gap-4">
      <BaseForm class="sm:grid-cols-2">
        <BaseFormItem class="sm:col-span-2" label="规则名称" required>
          <BaseInput v-model="draft.name" placeholder="例如：工作区仓库地址脱敏" aria-label="规则名称" maxlength="64" />
        </BaseFormItem>
        <BaseFormItem label="作用范围">
          <BaseSelect v-model="draft.scope" class="w-full" :options="scopes" aria-label="作用范围" />
        </BaseFormItem>
        <BaseFormItem label="处理方式">
          <BaseSelect v-model="draft.action" class="w-full" :options="actions" aria-label="处理方式" />
        </BaseFormItem>
        <BaseFormItem class="sm:col-span-2" :label="draft.scope === 'request_header' ? '请求头名称' : '字段路径'" required>
          <template #label-extra>
            <BasePopover trigger="hover-click">
              <template #trigger="{ open: helpOpen }">
                <BaseIconButton size="sm" variant="ghost" label="字段路径语法" :title="undefined" class="-my-2 hover:bg-transparent! active:bg-transparent!" :aria-expanded="helpOpen">
                  <CircleHelp class="size-3.5" />
                </BaseIconButton>
              </template>
              <div class="max-w-72 p-3 text-cp-sm text-cp-text-secondary">
                支持对象成员、双引号键名、数组索引和 * 通配，例如 $["workspaces"]["/home/alex/demo"]<br>环境文本使用 $，请求头直接填写名称
              </div>
            </BasePopover>
          </template>
          <div class="flex flex-wrap items-center gap-2 sm:flex-nowrap">
            <BaseInput v-model="draft.selector" :placeholder="selectorPlaceholder" aria-label="字段路径" maxlength="1024" class="min-w-0 flex-1 font-mono" :class="fields.length ? 'basis-full sm:basis-auto' : undefined" spellcheck="false" />
            <BaseSelect v-if="fields.length" :id="fieldPresetId" v-model="fieldChoice" class="min-w-0 flex-1 sm:w-40 sm:flex-none" :options="fields" aria-label="常用字段" />
          </div>
        </BaseFormItem>
        <BaseCheckbox v-if="draft.action === 'remove_field'" v-model="conditional" class="sm:col-span-2" label="仅删除值匹配正则的字段或数组项" show-label />
        <BaseFormItem v-if="usesPattern" :class="draft.action === 'remove_field' ? 'sm:col-span-2' : undefined" label="匹配表达式">
          <BaseInput v-model="pattern" :placeholder="draft.action === 'rename_key' ? '例如：^/home/[^/]+/' : '例如：private-org'" aria-label="匹配表达式" maxlength="4096" spellcheck="false" class="font-mono" />
        </BaseFormItem>
        <BaseFormItem v-if="draft.action === 'regex_replace' || draft.action === 'rename_key'" label="替换内容">
          <template #label-extra>
            <BasePopover trigger="hover-click">
              <template #trigger="{ open: helpOpen }">
                <BaseIconButton size="sm" variant="ghost" label="正则语法说明" :title="undefined" class="-my-2 hover:bg-transparent! active:bg-transparent!" :aria-expanded="helpOpen">
                  <CircleHelp class="size-3.5" />
                </BaseIconButton>
              </template>
              <div class="max-w-72 p-3 text-cp-sm text-cp-text-secondary">
                ${1} 或 ${name} 引用捕获组，$$ 表示字面 $<br>使用 Rust regex 语法，不支持前后顾和模式中的反向引用
              </div>
            </BasePopover>
          </template>
          <BaseInput v-model="draft.replacement" :placeholder="draft.action === 'rename_key' ? '例如：/home/user/' : '例如：team，留空则清除匹配内容'" aria-label="替换内容" maxlength="4096" spellcheck="false" class="font-mono" />
        </BaseFormItem>
        <BaseFormItem v-if="draft.action === 'set_value'" class="sm:col-span-2" label="固定值（JSON）">
          <BaseTextarea v-model="fixedValue" placeholder="例如：&quot;匿名&quot;、null 或 {}" :rows="2" maxlength="4096" aria-label="固定值 JSON" class="font-mono" />
        </BaseFormItem>
        <div v-if="usesPattern" class="flex flex-wrap gap-x-4 gap-y-2 sm:col-span-2">
          <BaseCheckbox v-if="draft.action !== 'remove_field'" v-model="draft.replaceAll" label="替换全部匹配" show-label />
          <BaseCheckbox v-model="draft.caseInsensitive" label="忽略大小写" show-label />
          <BaseCheckbox v-model="draft.multiLine" label="多行匹配" show-label />
        </div>
      </BaseForm>
      <section class="grid gap-2.5" aria-label="测试预览">
        <div class="flex items-center justify-between gap-3">
          <h3 class="m-0 text-cp-sm font-emphasis">
            替换预览
          </h3>
          <div class="flex items-center gap-2">
            <span v-if="outcome" class="text-cp-xs text-cp-text-quaternary">{{ outcome.status === 'skipped' ? '规则已跳过' : `命中 ${outcome.matches} 处` }}</span>
            <BaseButton size="sm" variant="ghost" :loading="loading" aria-label="测试本条规则" @click="test">
              <template #icon>
                <Play class="size-3.5" />
              </template>测试
            </BaseButton>
          </div>
        </div>
        <div class="grid min-w-0 gap-3 sm:grid-cols-2">
          <BaseFormItem label="测试样本">
            <BaseTextarea v-model="sample" size="sm" resize="none" placeholder="粘贴对应作用范围的样本内容" maxlength="262144" aria-label="测试样本" class="font-mono [&_textarea]:block [&_textarea]:h-36" spellcheck="false" />
          </BaseFormItem>
          <div class="grid min-w-0 content-start gap-2">
            <span class="flex min-h-4 items-center text-cp leading-none font-medium text-cp-text-secondary">处理后</span>
            <pre class="cp-scrollbar m-0 h-36 overflow-auto rounded-cp bg-cp-fill-quaternary p-3 font-mono text-cp-xs break-all whitespace-pre-wrap" data-preview-output>{{ processed || '点击测试查看结果' }}</pre>
          </div>
        </div>
        <p v-if="localError || error || outcome?.reason" role="alert" class="m-0 text-cp-sm text-cp-error-text">
          {{ localError || error || outcome?.reason }}
        </p>
      </section>
    </div>
    <template #footer>
      <BaseButton size="sm" variant="secondary" @click="open = false">
        取消
      </BaseButton>
      <BaseButton size="sm" variant="primary" :loading="loading" :disabled="disabled" @click="apply">
        应用到草稿
      </BaseButton>
    </template>
  </BaseModal>
</template>
