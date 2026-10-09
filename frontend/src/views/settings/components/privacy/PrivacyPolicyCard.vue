<script setup lang="ts">
import type { CodexPrivacyPolicy, PrivacyRule } from '@/api/modules/settings/privacy'
import { BaseButton, BaseCard, BaseIconButton, BaseMenuItem, BasePopover, BaseSelect, BaseSwitch } from '@codex-proxy/ui'
import { ChevronDown, CircleHelp, Play, Plus } from '@lucide/vue'
import { shallowRef } from 'vue'
import PolicyPreview from './PolicyPreview.vue'
import { newRule, presets } from './presets'
import RuleEditor from './RuleEditor.vue'
import RulesTable from './RulesTable.vue'

defineProps<{ disabled: boolean }>()
const model = defineModel<CodexPrivacyPolicy>({ required: true })
const editorOpen = shallowRef(false)
const selected = shallowRef<PrivacyRule | null>(null)
const isNew = shallowRef(false)
const presetOpen = shallowRef(false)
const testOpen = shallowRef(false)
const failureOptions: { value: CodexPrivacyPolicy['onError'], label: string }[] = [{ value: 'skip_rule', label: '跳过当前规则' }, { value: 'reject_request', label: '阻止请求发送' }]

function edit(rule: PrivacyRule, create = false) {
  selected.value = rule
  isNew.value = create
  editorOpen.value = true
  presetOpen.value = false
}
function apply(rule: PrivacyRule) {
  model.value = { ...model.value, rules: isNew.value ? [...model.value.rules, rule] : model.value.rules.map(item => item.id === rule.id ? rule : item) }
}
function add(values: Partial<PrivacyRule> = {}) {
  edit({ ...newRule(), ...values }, true)
}
function toggle(rule: PrivacyRule, enabled: boolean) {
  model.value = { ...model.value, rules: model.value.rules.map(item => item.id === rule.id ? { ...item, enabled } : item) }
}
function duplicate(rule: PrivacyRule) {
  if (model.value.rules.length >= 32)
    return
  model.value = { ...model.value, rules: [...model.value.rules, { ...rule, id: newRule().id, name: `${rule.name.slice(0, 60)}（副本）` }] }
}
function remove(rule: PrivacyRule) {
  model.value = { ...model.value, rules: model.value.rules.filter(item => item.id !== rule.id) }
}
function move(index: number, step: number) {
  const rules = [...model.value.rules]
  const target = index + step
  if (!rules[index] || !rules[target]) {
    return
  }
  const selectedRule = rules[index]!
  rules[index] = rules[target]!
  rules[target] = selectedRule
  model.value = { ...model.value, rules }
}
</script>

<template>
  <BaseCard id="privacy-policy" title="Codex 隐私策略" description="按自定义规则替换或移除请求中的隐私信息">
    <template #actions>
      <BaseSwitch v-model="model.enabled" label="启用请求规则" active-text="启用" inactive-text="关闭" inline-prompt :width="56" :disabled="disabled" />
    </template>
    <template #body>
      <div class="grid gap-3">
        <div class="flex flex-wrap items-center justify-between gap-2">
          <div class="flex flex-wrap items-center gap-2">
            <BaseButton size="sm" variant="secondary" :disabled="disabled || model.rules.length >= 32" @click="add()">
              <template #icon>
                <Plus class="size-3.5" />
              </template>添加规则
            </BaseButton>
            <BasePopover v-model="presetOpen" placement="bottom-start">
              <template #trigger>
                <BaseButton size="sm" variant="ghost" :disabled="disabled || model.rules.length >= 32">
                  从预设添加<ChevronDown class="ml-1 size-3" />
                </BaseButton>
              </template>
              <div class="w-48 p-1.5">
                <BaseMenuItem v-for="preset in presets" :key="preset.name" @click="add({ ...preset.values, name: preset.name })">
                  {{ preset.name }}
                </BaseMenuItem>
              </div>
            </BasePopover>
          </div>
          <BaseButton size="sm" variant="ghost" :disabled="disabled" @click="testOpen = true">
            <template #icon>
              <Play class="size-3.5" />
            </template>测试规则
          </BaseButton>
        </div>
        <RulesTable :rules="model.rules" :disabled="disabled" @edit="edit($event)" @toggle="toggle" @duplicate="duplicate" @remove="remove" @move="move" />
        <div v-if="model.rules.length" class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
          <span class="text-cp-xs text-cp-text-quaternary">按列表顺序执行</span>
          <div class="flex items-center gap-2">
            <span class="text-cp-xs text-cp-text-secondary">执行失败</span>
            <BaseSelect v-model="model.onError" size="sm" class="w-34" :options="failureOptions" aria-label="规则执行失败" :disabled="disabled" />
            <BasePopover trigger="hover-click" placement="top-end">
              <template #trigger="{ open: helpOpen }">
                <BaseIconButton size="sm" variant="ghost" label="规则执行说明" :title="undefined" class="hover:bg-transparent! active:bg-transparent!" :aria-expanded="helpOpen">
                  <CircleHelp class="size-3.5" />
                </BaseIconButton>
              </template>
              <div class="max-w-72 space-y-2 p-3 text-cp-sm text-cp-text-secondary">
                <p class="m-0">
                  跳过时撤销本条修改并继续执行，未完成本条脱敏
                </p><p class="m-0">
                  允许改写选定字段，认证、会话与工具行为的影响由配置者负责
                </p>
              </div>
            </BasePopover>
          </div>
        </div>
      </div>
    </template>
  </BaseCard>
  <RuleEditor v-model="editorOpen" :rule="selected" :is-new="isNew" :disabled="disabled" @apply="apply" />
  <PolicyPreview v-model="testOpen" :policy="model" />
</template>
